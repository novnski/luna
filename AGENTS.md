# LUNA development

Follow the parent workspace instructions. Work from this repository root, read `Cargo.toml` and the relevant crate before changing code, and preserve unrelated work. Use `--locked` for normal Cargo verification.

## Rust build and disk hygiene

- Prefer `cargo check --locked -p <package>` for fast feedback, then focused tests for the changed crate. Run broad workspace builds only when the change actually requires them. Avoid repeatedly compiling the large UI dependency graph for an engine-only change.
- Reuse this repository's `target/` cache; do not create a new `CARGO_TARGET_DIR` per task. If isolation requires a separate target directory, record its exact path and clean that directory when the task finishes. Serialize expensive Cargo builds that share a target directory.
- For one-off validation or CI-like runs, use `CARGO_INCREMENTAL=0` to avoid growing `target/debug/incremental`. Keep incremental compilation for an interactive edit loop when its faster rebuilds are worth the disk cost. Do not alternate build flags needlessly: changed profile or rustflags settings can cause extra rebuilds.
- At the end of every task, remove only task-created temporary build directories and disposable fixtures, then check `du -sh target/debug target/release` and available disk space. Preserve source, Git state, user data, packaged apps, and outputs another task is using.
- If `target/debug` exceeds **50 GiB**, coordinate with any active task first. Check that no Cargo/rustc process or open file is using it, run `cargo clean --dry-run --profile dev --target-dir` with this checkout's verified absolute `target` path, inspect the scope, then clean that **dev profile only** when it will not interrupt another task. Never run unqualified `cargo clean` or recursively delete the repository/workspace. Verify `du`, `df`, and `git status` afterward. Full clean is a milestone/size-pressure action, not an after-every-edit habit.
- If builds grow rapidly again, measure a dev-profile change before applying it. The Cargo Book recommends `debug = "line-tables-only"` for workspace crates and `debug = false` for dependencies, with a separate full-debug profile when debugger variables are needed. Preserve native-debugging needs and benchmark the trade-off before changing `Cargo.toml`.

References: [Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html), [build-performance guidance](https://doc.rust-lang.org/cargo/guide/build-performance.html), [profile settings](https://doc.rust-lang.org/cargo/reference/profiles.html), and [scoped `cargo clean`](https://doc.rust-lang.org/cargo/commands/cargo-clean.html).
