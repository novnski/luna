# Automation acceptance and test ownership

Written before the implementation. Existing pre-fix failures are retained in
`target/qa/automations-review-2026-10-09/probe-tests-final.log` and native date
readback. The owning runtime test remains `crates/engine/tests/automations_e2e.rs`;
extend its protocol peer to capture the real RunRequest and issue distinct
native session IDs, instead of adding a second overlapping test harness.

The independent contract is an approved, durable, project-scoped schedule and
its observable RPC/provider/transcript effects. Credible regressions include:

1. Missing fields break legacy stored JSON, or a legacy definition silently
   starts using the source chat's changed execution settings.
2. Save/preview loses explicit provider/model/reasoning, mixes model options,
   accepts malformed settings, or ignores the source permission ceiling.
3. A removed/deleted project, changed destination checkout, or archived existing
   destination still executes. A new-chat schedule unnecessarily depends on its
   archived/deleted source chat, or uses another project's folder/settings.
4. Cross-project retargets bypass approved scope or live source-run approval;
   UI preview and subsequent application describe different destinations.
5. Metadata read filters leak another project's/conversation's definitions,
   override includeDeleted, or paginate inconsistently after filtering.
6. Editing paused work resumes it. A finished one-shot stays Active. Changing a
   name resets PR wake history or changes approved cadence unexpectedly.
7. Unknown delivery is resent, or there is no explicit way to resolve the old
   hold for future work. A running occurrence is incorrectly resolvable.
8. Archive-successful-runs archives an existing chat, a failed run, a later human
   turn, or another task; terminal effects repeat across restart.
9. Notification Never still emits an automation alert, ImportantUpdates emits
   routine completion, AllRuns misses completion, or identity loses project
   attribution. Successful archive must not suppress durable history.
10. Daily/weekly/monthly/yearly custom recurrence ignores timezone/DST, loops on
    invalid rules, shifts interval anchors, duplicates a fold or consumes an
    unbounded backlog. Invalid/unsupported input must be rejected clearly.
11. Automation tools accept a different schema from the UI/RPC, an agent cannot
    discover legal targets/models, or tool results lack the inline card data.
12. UI lists the previous project, fake model options, missing trigger labels,
    unavailable history after deletion, or wraps malformed date integers.

Each behavior has one strongest proof: real RPC + SQLite + task admission +
scripted provider for runtime; native/headless UI E2E for controls and messages;
bounded real-provider scheduled smoke for adapter acceptance. No test-only
production accessor, exported seam or duplicate unit test is required. Retain
commands, exit codes, JSON/captures and hashes. Do not claim mock provider proof
as a real model result. Codex coordinates all shared builds and app slots.

## Live-provider extension

The existing `scripts/qa-automations-live.mjs` owns native scheduler smoke QA.
Extend it before the final app run to cover explicit execution selection and
optional OpenCode/fresh-chat modes. Its independent failure cases are a rejected
real model/reasoning selection, delivery to the source rather than a fresh chat,
missing/duplicate scheduled messages, paused work firing, or completed work
replaying after restart. Persist catalog, exact preview/receipt, observed runtime
settings and transcript; distinguish admitted settings from native provider echo.
Keep model calls to marker-only replies without tools. No new production seam.

## Acceptance timing regression found by rendered QA

`waiting_for_provider_acceptance_does_not_report_unknown_delivery` is owned by
the canonical engine E2E suite. An admitted command uses Pending plus
ProviderAcceptanceUnknown while correlation is still awaited; that placeholder
must not become an automation Unknown while the provider is live, even past the
startup grace period. A committed ambiguous receipt, engine restart, or a run
that stops without acknowledgement must still preserve genuine uncertainty.
The peer holds its first SessionStarted event; the test ticks reconciliation,
then releases acknowledgement and requires one completed occurrence. The old
code failed with Unknown instead of Dispatching (`pending-before.log`). Existing
protocol-loss tests cover stopped work; the live Resolve test now explicitly
closes the command as ambiguous through the production DocHost boundary.
