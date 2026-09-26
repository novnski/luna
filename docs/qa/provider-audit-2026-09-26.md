# Provider audit — 26 September 2026

Base: `7b141044c10650cc2ec476128763384cf5084cfe`.
Worktree: `/Users/iulian/.codex/worktrees/e577/luna`.
Branch: `codex/audit-provider-integrations`.

This bounded pass covers provider lifecycle, catalog discovery, account
selection, and their adjacent runtime behavior. It preserves all visual styles.
The seven fixes in `backend-audit-2026-09-26.md` are the starting checkpoint.
Infrastructure commit `d1c71c3a` was incorporated before the first build.

Author provenance: `a61dd1bbc` introduced the classified Codex steer handling
and Claude write acknowledgement. Usage capture/refresh traces to `8074b4c0b`
and `ed57c96a3`. The first-turn wait and model parser are inherited code; the
parser became authoritative through the later provider/catalog consolidation
and caching work. They are included because they affect these connected paths.

## Failure modes recorded before implementation

1. Codex awaits the first `turn/start` response outside the interrupt selector.
   A provider that stops replying keeps both Stop and a dropped consumer from
   reaping the subprocess. The same gap exists in idle follow-up starts and
   `turn/steer`. Initialization observes Stop but not a closed consumer.
2. Claude waits for the explicit steer's stdin-write ACK inside its event-loop
   branch. Backpressure from a provider that stops reading stdin prevents Stop
   or consumer teardown from reaching process cleanup.
3. Ordinary Codex steering retries every `request_classified` error. If a peer
   accepts the steer, queues `turn/completed`, closes stdout before its ACK,
   and keeps stdin open, an unknown outcome becomes a second `turn/start`.
4. Codex model discovery treats missing/non-array `data` as an authoritative
   empty catalog and a repeated pagination cursor as an authoritative partial
   catalog. Both results can replace the account's last-known-good cache.
5. Usage captures the selected Codex transcript home before asynchronous work,
   but quota resolves the selection after the history scan. Account changes
   during that interval can publish stale history or combine two accounts.
   Requests excluding limits have no account-selection fence at all.

## Regressions authored first

`crates/harness/tests/provider_lifecycle_audit.rs` uses disposable subprocess
peers. The peer writes exact protocol-boundary markers; tests cancel or drop
the consumer only after the requested boundary is reached. They check terminal
events, uncertain steer acceptance, absence of replay, and exact PID removal.
The Claude peer consumes one byte of a large steer before stopping its reads,
which establishes dispatch without relying on sleeps. Failure cleanup kills
only the captured disposable peer PID.

`crates/engine/tests/usage_account_selection.rs` calls the production RPC and
account APIs. A runtime with one occupied blocking worker queues the history
scan after capturing account A; account A is then disconnected before the
worker is released. The transcript, credentials placeholder, price cache,
engine data, and provider homes are all disposable. The test uses no scan hook,
real account, or price-network request.

## Verification

Baseline artifacts are under `target/qa/provider-audit-2026-09-26/reproductions/`:

- `harness-before.log`: **1 passed, 12 failed**. The lost-reply case records the
  duplicate `turn/start` containing `redirect once`; cancellation failures show
  the disposable peer still alive. The initialization interrupt control passed.
- `usage-before.log`: compile-only test harness failure (missing trait import),
  not a runtime reproduction. It is retained separately for full provenance.
- `usage-before-rerun.log`: the actual unchanged-handler reproduction returned
  old account A's **40 tokens** after disconnect with `includeLimits=false`.
- `account-warning-before.log`: the existing Claude identity fixture failed
  because its all-provider listing included the expected Codex native-login
  warning. The corrected assertion checks Claude warnings specifically.

The corrections keep provider waits responsive to cancellation and consumer
closure, preserve unknown steering outcomes without replay, reject malformed
or cyclic catalogs, and capture one Codex home/auth mode for history and quota.
Usage checks the selection before starting quota and again before publication;
a change returns an explicit retry error, including history-only requests.

Run `scripts/qa-provider-audit.sh` with a coordinator build slot. The runner
uses the guarded Cargo wrapper and retains commands, logs, times, exit codes,
and cache reports. Its first failure preserves the original exit code,
including guard exit 75.

Fixed results are retained in `target/qa/provider-audit-2026-09-26/fixed/`:

| Check | Result |
| --- | --- |
| Provider failure-mode regressions | 13 passed; every disposable peer reaped |
| Usage account selection | Passed for history-only and combined requests |
| Full harness suite | 237 passed, 9 opt-in tests ignored |
| Existing account tests | 21 passed |
| Account lifecycle, model refresh, provider scope, accounts/uploads/titles | 14 passed |
| Locked engine/harness check | Passed |
| Formatting, whitespace, script syntax and ShellCheck | Passed |
| Clippy, all targets | Passed after the fixture corrections below |

There are **273 distinct passing runtime tests**. The focused provider rerun
and two fixture reruns produce **288 successful test executions** in the logs.

Clippy's first invocation stopped on two inherited OpenCode HTTP fixtures
(`4902c7925`) that discarded `socket.read`'s byte count. Those existing fixtures
now assert that request bytes arrived before returning their synthetic HTTP
response. No runtime code changed for this validation repair. Both exact tests
passed individually afterward. `clippy.log` preserves the failures and
`clippy-rerun.log` preserves the passing retry; existing unrelated warnings
remain visible. No new unit tests were added after implementation.

The new subprocess fixtures use no real provider account. Claude and OpenCode
coverage is synthetic. Real Codex smoke and native-app readback remain pending
a separate coordinator slot. This bounded pass does not establish Windows or
Linux native-app behavior or exhaustively validate every provider contract.

Reserved app: **LUNA QA Providers**, `com.novnski.luna.qa.providers`, IPC `59781`.
After the code verification, `target/debug` is **4.1 GiB**, with **427 GiB** free
on the volume. Temporary provider homes/peers were removed by the fixtures;
the worktree cache and QA evidence remain available for review.
