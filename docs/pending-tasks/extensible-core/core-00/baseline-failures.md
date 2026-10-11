# Baseline failures and exclusions

These are separate from plugin migration regressions. Raw attempts are retained
under `target/qa/core-00`; the evidence manifest identifies exact files/hashes.

| Observation | Disposition / strongest owner |
| --- | --- |
| Existing `local_profiles`, `restart_resume`, `task_commands`, `timeline_order_e2e`: 58 passed, one ignored | Fresh current-source result. The ignored `real_claude_remembers_codeword_across_engine_restart` requires an authenticated Claude CLI and spends tokens; controlled restart proof passed. Live provider coverage remains SW-00. |
| Legacy native resource-profile attempt lost foreground focus | Invalid performance run; no numbers accepted. Its copied pre-existing executable was not a newly verified current build. Preserve `legacy-profiler.log`, never use this as a migration comparison. |
| Current offscreen profiler panicked installing native quit delegate | `BenchPlatform` has no AppKit delegate. Fixed the example startup to omit the native application menu/quit hook; production shutdown stays unchanged. Exact pre-fix panic retained in `replay.log`. |
| Current offscreen profiler lacked `HistoryColumnPreferences` | Rebased the example initialization against current `run_app` and native fixtures: initialize `gpui_base` and history preferences. No new production export or test-only seam. Pre-fix panic retained in `replay-2.log`. |
| First successful replay's pixels showed startup overlay and all rows settled | The old tool captured immediately, used stale fixture dates and lacked local device identity. It now captures after settling, seeds recent chat activity/device identity, and labels a real scroll-input phase separately from idle draw-loop overhead. `replay-final` is superseded for UI performance; the corrected replay owns the UI baseline. |
| First benchmark executable copy exited by SIGKILL before main | Copying the bundle-signed Mach-O outside its bundle needs its own ad-hoc signature. Runner now seals/verifies only its private copy and records source and measured hashes. Failed `--help` returned -9; original bundle returned 0. No app/core repair was made. |
| First concurrency probe sampled before runs became Working; mock run receipts can be pending | Runner now waits for independently observed overlap and validates completed transcript rather than calling admission completion. Only the specific `providerAcceptanceUnknown` pending reason is allowed. This is the controlled mock's correlation limitation; production-provider semantics are not redefined. |
| Early Wasmtime compile used the removed default error integration | Selected minimal features now explicitly include `anyhow`; removed deprecated `async_support` call. Final standalone lockfile/build/lint prove the pinned API. Initial build failures remain in raw logs. |
| Existing compiler/Clippy warnings across owned crates/vendor | Recorded by the selected builds; no broad warning sweep in CORE-00. The new isolated runtime is checked with `-D warnings`. |
| Hosted queue CI had zero executed steps | Run 38099968701 was still failed at execution start, matching the retained billing/spending blocker. No account/billing settings changed. New workflow configuration is not hosted passing evidence. |

The first engine benchmark used SIGTERM and did not retain executable names for
all resource samples. The final runner uses `StopEngine`, owns a distinct process
group, verifies group cleanup and records per-process executables. Only the final
run owns the comparison baseline; earlier passes remain useful harness evidence.
Process counters are sampled, so children that start and exit between samples
can be missed. Sums of charged footprints are not physical-exclusive memory.
