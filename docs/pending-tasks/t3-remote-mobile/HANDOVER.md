# Implementation handover

Read [README](README.md), [gap matrix](gap-matrix.md), [tasks](implementation-plan.md), [failures/E2E](validation-plan.md), and [verification](verification.md). Detailed reports preserve exact source evidence for every subsystem.

Start with RM-00 against current source, then implement the existing Control this Mac M0–M4 sequence using RM-01–RM-07 as bounded task detail. Preserve direct-only/no-relay V1, one authoritative Mac engine/profile, pinned native TLS, current iOS identity/data, host-owned credentials and the existing native stack decision. T3 source inventory is 2.0.0-era upstream; stable store1.4.1 parity was not tested.

Priority correctness requirements: typed authenticated device principal at host admission; method+nested-intent+target authorization; durable stable-ID receipt before acknowledging; exact local executable grant protection; stale request/queue revisions; cache/stream freshness; device revoke/disable that has specified asset semantics. Do not solve remote task operation by allowlisting ExecuteTaskCommand alone or writing synced rows.

Write credible failures first. Prefer existing strongest-boundary E2E owners, produce retained repeatable artifacts, then run scoped locked wrapper/build/format checks and review. After meaningful Mac changes, refresh LUNA Dev yourself and verify exact process, IPC listener and signature. Respect single-app/QA identity/build-budget/graceful-quit rules. Use commit-and-push after complete milestone checks.

Later RM-08–RM-13 remain pending; this task authorizes research/documentation, not cloud infrastructure or app-feature implementation. Link existing HTML/MCP Apps, Board/Side chats, scheduler, Git, terminal, browser/device and updater owners. Do not create duplicate engines, schedulers or renderers.

This package's tests are proposed. Fresh live physical-phone/provider/network proof is required before a feature is called operational.
