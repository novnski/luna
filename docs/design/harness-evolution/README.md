# Harness evolution

Implementation started 2026-10-05 at `83ec2a12`, following the October 4 T3 V2
and BB research. Keep Luna's Rust engine, native adapters, synchronized chat
documents, task identities, command receipts and acceptance-aware recovery.

## Reconciliation

The research baseline was `0800dfc6`. Since then, native forks became ordinary
persistent chats, workbench creation gained explicit New thread / Side chat
choices, lineage and captured-context help were unified, Settings adopted the
contained surface, and composer/tab/group keyboard routing was verified.
Those paths are foundations, not missing features to rebuild.

Current source already separates requested and effective permission modes,
retires incompatible warm runtimes, verifies Claude live model changes, and
preserves unknown native steering acceptance without replay. Native history,
child lifecycle, context boundaries and runtime instruction receipts remain
authoritative.

## Delivery sequence

1. Adapter-derived capability and task execution diagnostics; pending setting
   changes and native steering admission; focused-chat UI and E2E evidence.
2. Budgeted visible-text context transfer and fork-result conversation merge.
3. Background-work status, lifecycle ownership, scoped stopping and result
   delivery, preserving the difference between ancestry and ownership.
4. Repair durable-effect boundaries only when the failure matrix proves a gap.
5. Durable application scheduling, followed by PR-triggered wakeups.
6. Measure long histories and pane work before performance changes; keep input
   routing tied to the owning composer.

Each completed slice receives focused verification, a primary LUNA Dev refresh,
process/IPC/signature readback, scoped QA cleanup and verified Git delivery.
Codex owns engine/proto changes and verification. The established Claude
conversation owns coordinated UI changes. Shared builds and GUI launches are
serialized. Unrelated pending-task research remains untouched.

Delivered evidence: [execution diagnostics](verification.md) and
[portable context and fork findings](slice-2-verification.md), and
[shared background work](slice-3-verification.md), and
[durable dispatch boundaries](slice-4-verification.md), and
[host-owned automations](slice-5-verification.md). The remaining performance
slice continues with Codex and Claude as the live acceptance targets.

## Slice 1 failure cases, specified before implementation

- Adapter declarations are mistaken for negotiated native support, or missing
  evidence is displayed as supported/verified.
- Requested model, reasoning, options or permissions replace the displayed
  running selection before the provider has begun using the new settings.
- Inherited defaults are mistaken for changed settings because a requested
  value is absent while a resolved runtime value is present.
- Pending permission/model/account/workspace changes are silently ignored by
  native steering; a newly submitted message runs under obsolete policy.
- Rejecting a stale steer loses the queued message, duplicates its transcript
  entry, interrupts unrelated work, or authorizes replay after unknown delivery.
- Permission changes in both directions are reported as applied during the old
  turn. Parent ceilings or a read-only sandbox disappear during reconciliation.
- The planner promises a native model change that the adapter would reject, or
  promises automatic history transfer on a new binding without an explicit
  transfer. Preview and execution must share adapter decision functions.
- An account changes while retaining the same model name and gets warm reuse.
- Unsupported/retained model options or a stale catalog are presented as
  provider-acknowledged. A failed native settings readback stays unknown/failed.
- A cold or restarted engine displays historical observations as a live run.
- A remote task receives the local machine's capabilities or effective state.
- Watch updates fail to refresh the current snapshot on configuration, start,
  completion or retirement, or old responses overwrite a newly focused tab.
- Runtime diagnostics follow workbench Main instead of the focused chat.
- Private model options, instructions, credentials or provider-native IDs leak
  into the public snapshot or UI.
- A read-only diagnostic request starts a provider, changes configuration or
  advances a receipt. Older engines without the additive field remain usable.

Evidence lives under `target/qa/harness-evolution/`; existing milestones remain
unchanged. Use RPC/persistence/native-protocol and UI interaction evidence.
Do not add unit tests after implementation.

Native acceptance must reject a mismatched bundle, port or data profile before
any mutation. In a disposable project, a real Codex and Claude turn performs
only a bounded sleep and prints a marker. Changing Full to Ask while it runs
must preserve Full in the running observation, show Ask as requested, hold a
Steer message without delivering it, and use Ask when that exact queued message
is explicitly sent after the turn ends. The message must appear once. Timeouts
retain their manifest and never cause an automatic replay. Restart readback
must show no live runtime. Provider catalogs are discovered live; no credentials
are copied into the fixture or evidence.
