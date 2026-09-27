# Error feedback native QA — 2026-09-27

These screenshots come from isolated GPUI fixtures with synthetic tasks and drafts. Provider homes and authentication are isolated; sound and desktop notifications are disabled in the Activity fixture. The full local run logs and signed-app readback remain under `target/qa/error-ux-2026-09-27/`.

- [Normal sidebar](sidebar.png): no in-app completion card.
- [Activity view](activity.png): tasks needing attention plus a recent completed task, with no duplicate completion card.
- [Draft recovery before](draft-recovery-before.png): the generic red failure repeats the yellow recovery card.
- [Draft recovery after](draft-recovery.png): one neutral saved-message card with explicit Restore and Discard actions.

The final Activity and draft fixtures exited successfully. `cargo fmt --all --check`, `scripts/cargo.sh check --locked -p luna-ui`, and `scripts/cargo.sh clippy --locked -p luna-ui` succeeded. The macOS developer app refreshed in 42 seconds; its exact bundle process, IPC listener on port 59777, and strict code signature were verified. The revision race and transient draft-save failure were not fault-injected in a native fixture.

## Repeat

From the repository root, with no other Luna GUI app running:

```sh
scripts/cargo.sh build --locked -p luna-ui --example activity-attention-fixture --features appshots-fixture
scripts/cargo.sh build --locked -p luna-ui --example drafts-fixture --features appshots-fixture
mkdir -p /private/tmp/luna-l15-error-feedback-repeat /private/tmp/luna-l08-error-feedback-repeat /private/tmp/luna-error-feedback-repeat-output
target/debug/examples/activity-attention-fixture /private/tmp/luna-l15-error-feedback-repeat /private/tmp/luna-error-feedback-repeat-output
LUNA_FIXTURE_EXIT_AFTER_CAPTURE=1 target/debug/examples/drafts-fixture /private/tmp/luna-l08-error-feedback-repeat recovery /private/tmp/luna-error-feedback-repeat-output/draft-recovery.png
```

The Activity fixture writes `sidebar.png` and `activity.png` and exits after capture. The draft fixture writes `draft-recovery.png` and exits when the environment flag is set. Captures can differ in timestamps or task ordering; `SHA256SUMS` verifies the archived screenshots, not byte identity with a new run.
