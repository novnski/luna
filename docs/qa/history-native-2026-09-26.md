# Combined audit and Usage native verification — 26 September 2026

Verified source: `8169b69219f5d1b598f64189a8e2b1b937aa3d7c`, adopted with
`git merge --ff-only` on the clean `codex/audit-history-preferences` branch.
This is the coordinator-reviewed combination of all four audits and the
shared fixture correction. No production source changed during this run.

## Combined engine batch

One guarded command, with `CARGO_INCREMENTAL=0`:

```sh
scripts/cargo.sh test --locked -p luna-engine \
  --test usage_history --test usage_account_selection \
  --test remote_doc_authority --test workspace_data_safety \
  --test drafts --test message_queue
```

**62 passed, zero failed, zero ignored**, in 48 seconds including compilation.
Counts were 11 usage-history, 1 account-selection (both request variants),
5 remote-authority, 9 workspace-safety, 3 draft and 33 message-queue cases.
The earlier author-focused baseline failures and the intermediate fixed-run
failure remain documented in `history-audit-2026-09-26.md`; this combined run
does not replace or relabel that evidence.

## Isolated native app

Built using `scripts/run-macos-dev.sh --refresh`, `CARGO_INCREMENTAL=0`, and
all four assigned isolation settings:

| Property | Verified value |
| --- | --- |
| App | `LUNA QA History` |
| Bundle ID | `com.novnski.luna.qa.history` |
| IPC | `127.0.0.1:59784` |
| Data | `/Users/iulian/.codex/worktrees/1e48/luna/target/luna-qa-data/history` |
| Executable | `/Users/iulian/.codex/worktrees/1e48/luna/target/luna-macos-dev/LUNA QA History.app/Contents/MacOS/luna` |
| Build time | Cargo 129 seconds; complete refresh 130 seconds |
| Initial / restarted PID | `88346` / `90021` |
| Signed executable SHA-256 | `d6baaf1ec3d75468c53b6cb7c6042143fb6838f73df9a77b47222fb65d6aefb4` |

Exact executable and listener ownership were read back before and after a
graceful QA-only restart. Strict deep `codesign` verification passed both
times; the binary hash remained unchanged. The shared build slot was released
as soon as the native build completed. Subsequent verification used IPC and
the existing bundle only, with no further Cargo commands.

## Real historical Usage and persistence

Three bounded IPC calls used the same closed 24-hour window ending five
minutes before the first request. Limits were disabled; no account switching
or provider session creation occurred. Artifacts retain aggregate results,
not provider prompts, messages, session identities or source paths.

| Read | Duration | Files scanned | Files cached | Result |
| --- | --- | --- | --- | --- |
| Cold | 17,083 ms | 112 | 0 | Complete |
| Same-process repeat | 93 ms | 0 | 112 | Complete |
| After graceful app restart | 585 ms | 4 | 108 | Complete |

Every read returned the same **910,320,824 tokens**, 37 buckets and 5 models
across 2 ready historical sources. All bucket boundaries matched the request;
there were zero failed files, unpriced records or warnings. The four files
rescanned after restart did not change totals in the closed request window.

The assigned QA data directory contains a 702,265-byte pricing cache and a
14,504,478-byte scan cache after the final read. The pricing cache's timestamp
was unchanged across repeats. The restarted process's cached-file count
confirms that history was reloaded from disk.

Eight representative provider transcripts, totaling 3,866,246 bytes and last
modified over six hours earlier, had identical content hashes before and
after both the initial pair and restart read. This is a bounded integrity
sample, not a claim that concurrently active provider files never change.
The primary LUNA Dev executable hash was unchanged; no matching primary
bundle process was present in either recorded process snapshot. Only the
verified QA PID was asked to quit.

## Artifacts, storage and limits

All artifacts are retained at
`target/qa/history-native-8169b692/`: `source-sha.txt`, `combined.command`,
`combined.log`, `combined-counts.json`, `combined-result.txt`, cache/budget
reports, `native-build.command`, `native-build.log`, `native-cargo.log`,
`native-manifest.json`, bundle/signature/process/listener readbacks,
`usage-request.json`, the three `usage-*.json` summaries, provider integrity
summaries, and main-before/main-after evidence.

Repeatable readback tools are retained beside those artifacts:
`usage-readback.mjs`, `provider-readonly.py` and `reload.sh`. The latter
gracefully restarts only the verified QA executable and performs no build.
They require this checkout, its running QA app and the assigned port/data.

Final debug target: approximately **9.2 GiB**; no release target or additional
build target was created. Aggregate guard accounting was **112.91 GiB before**
and **127.81 GiB at final readback**, below the 170 GiB operating ceiling.
Other authorized workers could build after slot release, so the aggregate
difference is not attributed solely to this app. Available disk space was
approximately 409 GiB. Disposable Python bytecode was removed; the app, data,
Cargo cache and QA artifacts remain available for review.

This verifies the requested combined engine paths and macOS Usage readback.
It does not add native UI coverage for unrelated features, repeat the full
audit suites, or create live provider conversations. No push or main merge
was performed by this worker.
