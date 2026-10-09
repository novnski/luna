# Project selector device names

- `before.png`: user-supplied macOS screenshot showing missing host labels.
- `after.png`: native Linux/X11 capture of the production Shell with synthetic data, showing duplicate project names, an offline host, long labels, and the unknown-device fallback.
- `filtered.png`: the same native fixture after searching for `anara`.

These captures predate the sidebar redesign, which moved the machine label from the trigger into the dropdown. The `project-selector-fixture` that produced them was retired in the 2026-10-09 test audit; `crates/ui/examples/sidebar-polish-fixture.rs` now owns the device-label cases (`anara @ Studio Mac`, `anara @ Build server`) and writes a pass receipt.

The fixture uses temporary settings and in-memory data without starting an engine or connecting to a real device. These captures validate rendering and picker interaction, not device synchronization. The after captures use Linux window rendering; they do not validate the macOS compositor.

Validation: `cargo build -p luna`; `cargo build -p luna-ui --example project-selector-fixture`; `cargo test -p luna-ui --lib state::tests:: -- --test-threads=1` (54 passed); rustfmt checks on the changed Rust files; `git diff --check`.
