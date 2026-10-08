# Usage background refresh review

Scope: Providers, Usage and the sidebar usage pop-up. Keep manual refresh only
on the two Settings pages; the pop-up must read cached data immediately and
never display a refresh action or refresh-failure/staleness warning.

## Observed before implementation

- Real primary app RPC: history 5,094 ms; full usage 6,190 ms; repeated full
  usage 5,889 ms. Account listing took 791 ms.
- Claude reported unsupported because `.credentials.json` was absent, even
  though the native account was connected. The reader never asked Claude's SDK.
- Profiling found 2,869 of 3,042 sampled scanner frames in full-cache JSON
  serialization after just three changed files and 2,571 cached files.
- Opening history-only Usage replaces the Shell snapshot with `limits: []`.
  That removes pop-up quota data until its visibility-gated refresh runs again.
- Providers clears its account/usage view to Loading on every refresh, and
  account usage misses replace successful cache entries.
- OpenCode's reader selects the newest Go key without `active`, and does not
  accept device OAuth credentials or the `opencode` integration's saved rows.

Local reference sources (read-only): T3 `ClaudeProvider.ts` uses the native
`get_usage` control; `makeManagedServerProvider.ts` separates published reads
from background probes. BB `plugins/provider-usage/server.ts` retains measurements
and coalesces requests; `opencode-usage.ts` selects active saved key/device OAuth
credentials. BB's direct Keychain reader is not needed: use Claude's own native
process, with no prompt, hooks, MCP servers or application credential extraction.

## Failure cases written before changes

1. Cold or warm quota reads wait for transcript scans, price downloads, another
   provider, or an offline environment. A cache read must return promptly while
   work remains in the background; successful providers publish independently.
2. Closing the pop-up or leaving Settings stops updates. Engine-owned background
   refresh must continue without UI demand and stop with the engine.
3. Concurrent readers duplicate provider probes; one cancelled reader aborts
   shared work; a failed probe retries on every render. Coalesce and bound work.
4. A transient failure clears last-good percentages or provider tabs. Preserve
   values and their original observation time, without claiming they are fresh.
5. Account, provider home, executable wrapper, enablement or selected OpenCode
   credential changes reuse another account's cached data. Fence cache entries
   and in-flight publication by the exact selected scope; never invent identity.
6. History-only reads erase limits, or limits-only reads erase history. Keep
   these lanes independent in the engine and when merging UI responses.
7. Claude usage requires a credentials file, launches a model turn, runs hooks,
   accesses unrelated MCP services, or reveals auth material. Use only native
   initialization plus correlated `get_usage`, with bounded clean teardown.
8. Inactive OpenCode credentials beat active ones; OAuth is treated as a key;
   custom issuers receive credentials. Respect current schema/precedence and
   keep official-origin and account-change checks.
9. Refreshing Providers replaces valid rows with skeletons; failed metadata
   reloads erase account rows. Keep the last complete view during refresh.
10. The pop-up has Refresh, stale/refresh-error/unsupported warnings, a live
    countdown on old observations, or fabricated zero usage. Use last-reported
    values with quiet provenance; no-data placeholders must not imply zero.
11. A tiny transcript append serializes the whole cache. Persist changed cache
    entries incrementally; recover partial writes without losing cached history,
    and preserve existing migration/deduplication/retention behavior.
12. Cached summaries survive restart under the wrong account or period/prices.
    Persist only bounded data with scope and query keys; preserve exact ranges.

## Acceptance

Use public RPC E2E with isolated on-disk fixtures for background reads, refresh
coalescing, independent lanes, recovery and persistence. Keep existing account
selection/history regressions. Verify real native Claude and OpenCode quota
reads, then measure cold/warm pop-up and history responses. Render and inspect
the real UI with fresh, retained and empty data. Record timings, native captures,
logs and checksums under `target/qa/usage-refresh-20261008/`.

## Findings and implementation

Read-only comparison used local T3 Code `4ee6bfd50e` and BB `a2bbfaad`.
T3's `apps/server/src/provider/Layers/ClaudeProvider.ts` initializes the native
SDK and requests `get_usage`; `provider/makeManagedServerProvider.ts` publishes
cached provider snapshots independently of background enrichment. BB's
`plugins/provider-usage/server.ts` coalesces measurements, and
`packages/provider-bridge-acp/src/bridge/opencode-usage.ts` respects active
credential rows and native device OAuth under both OpenCode integration IDs.

LUNA previously needed 5,094 ms for history, 6,190 ms for a combined read and
5,889 ms for the subsequent combined read. Accounts took 791 ms. A sampling
profile attributed 2,869 of 3,042 scanner samples to serializing the entire
history cache, despite only three changed files and 2,571 cached files.
Claude was reported unsupported because its login was in native macOS storage.

The engine now owns independently refreshed quota and history caches, shares
in-flight work, preserves same-account last-good observations, and persists
bounded summaries. Native quota checks run without UI demand. Reads expose
background progress without waiting for the probes. Account/profile changes
fence both in-flight results and UI merging. History writes use a checkpoint
plus changed-entry journal with generation and checkpoint-digest matching.
Claude uses initialization and `get_usage` only, with no user turn, hooks, MCP,
or session persistence. OpenCode selection handles active key/API/device OAuth
credentials and validates the official issuer. UI history reads preserve quota
state, Providers retains valid rows during refresh, and the compact popup has
no Refresh control or refresh/error warning footer.

## Automated and native acceptance

- Public RPC integration: all 13 tests passed across `usage_refresh`,
  `usage_history`, and `usage_account_selection`. The new E2E first failed on
  the old RPC contract, then passed after the fix. It covers cold background
  reads, quota reads while the history worker is blocked, forced refresh,
  transient failure retention, independent lanes, and restart persistence.
- Existing UI suite: 1,188 passed. Existing engine usage checks: 29 passed,
  three ignored. No new unit tests were added after implementation.
- Native cache E2E: Codex, Claude and OpenCode Go all returned ready windows;
  first and warm cached reads took 39 ms and 29 ms. After 82 seconds with no
  usage request, all three observation timestamps advanced; the read took
  42 ms. OpenCode used the existing isolated Go QA profile, without copying
  credentials. See `native-cache-background.json` for the exact observations.
- Journal recovery E2E: four separate scanner processes verified 20, 40, then
  60 tokens, with the base checkpoint unchanged after appends. A torn invalid
  UTF-8 journal tail did not prevent later writes from replaying. Removing the
  source transcript still retained all 60 tokens on restart. See
  `journal-recovery.json`.
- Primary LUNA Dev native acceptance: all three normal provider profiles are
  ready (Codex one window, Claude three, OpenCode Go three). Cached quota and
  combined reads took 55 ms and 29 ms, warm history 15 ms, and Providers 33 ms.
  A forced history recomputation took 665 ms, with 2,572 transcript files reused
  from cache. This is approximately 7.7 times faster than the 5,094 ms baseline;
  cached combined reads are approximately 200 times faster than the 5,889 ms
  previous warm combined read. Measurements are local observations, not a
  network-latency guarantee. See `after.json`.
- Native screenshots confirm populated Claude/OpenCode popup bars with no
  Refresh control or refresh warning; populated provider rows survive manual
  refresh; Usage renders 625 historical sessions and all three providers'
  limits; opening the popup after history navigation preserves its quota bars.
- Build succeeded in 119 seconds. Exact primary bundle process, loopback IPC
  `59777`, and `codesign --verify --deep --strict` passed. Clippy completed for
  engine, harness and UI with existing repository warnings; the final engine
  rerun has no warnings on the new cache or OpenCode-selection code. Formatting
  and `git diff --check` passed. No separate QA app or new checkout was created.

Repeat the isolated checks with:

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test usage_refresh --test usage_history --test usage_account_selection
CARGO_INCREMENTAL=0 scripts/cargo.sh run --locked -p luna-engine \
  --example usage_cache_readback -- /absolute/output.json --journal-check
# Uses the caller's selected native provider profiles; no model turns are sent.
CARGO_INCREMENTAL=0 scripts/cargo.sh run --locked -p luna-engine \
  --example usage_cache_readback -- /absolute/native-output.json --watch
```
