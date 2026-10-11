# CORE-02 failure cases (E04)

These were written before any test or production change. Each names how it is
observed and which artifact proves it.

| ID | Credible failure | Expected behaviour | Observed by |
| --- | --- | --- | --- |
| K1 | A keeper disappears while tests move, merge or retire (E04: "moved keeper lost"). | Every C/D row in the 9 October ledgers still has its named keeper, or a recorded replacement. CORE-02 adds no new missing keeper. | `scripts/qa-test-ledger-rebase.py` before and after; the rename and replacement table. |
| K2 | Retiring dead code removes the only test of a live production rule. Example: projection-lock ordering is tested only through the dormant interrupt path. | The rule is re-homed on its production path first. After retirement, a mutation of the rule turns the re-homed test red. | Before/after engine runs; mutation log. |
| K3 | A "dormant" path still has a production caller, so retiring it breaks the product. | Callers are checked before deletion. The engine, harness, UI and app still build, and their suites match the baseline. | `rg` caller evidence per item; before/after suite counts. |
| K4 | A baseline failure is dismissed as environment or flakiness, or a new failure hides among old ones. | Each baseline failure is recorded with its cause and an isolated rerun. A genuine regression blocks delivery. | Baseline ledger with commands, exit codes and isolated reruns. |
| K5 | A routed example test can't fail: its fixture supplies the answer, or the example default `test = false` means it never runs. | Each routed example reports a nonzero test count. One selected fixture contract fails under a production mutation. | Local run counts; CI log counts; mutation log. |
| K6 | CI routing looks right but nothing executes: a wrong flag, a missing feature, or the job never starts. | GitHub Actions runs the new steps on the delivered commit and reports their test counts. | Actual run id, step conclusions and counts. |
| K7 | A test-only export or `_for_tests` seam outlives the test that needed it. | After retirement, no orphaned test-only item remains for the retired paths. Clippy reports no new dead code. | `rg` for the retired symbols; Clippy log. |
| K8 | Distinct provider peers (Codex, Claude, OpenCode) are merged into one fake, or provider wire tests are dropped. | Provider peers and wire/parser contracts are untouched; the harness suite count matches the baseline. | Harness before/after counts. |
| K9 | A ledger "retain" row was renamed by a lane's own consolidation, and the rebase counts it as lost (or hides a real loss). | Each retained-but-absent row maps to its replacement test, or to an explicit later product change. | Rebase trace table. |
