# Slice 2: explicit portable conversation context

Extend the existing task-command/context-boundary transaction. A read-only
host preview resolves visible, completed user/assistant text and computes a
content-addressed preview ID from source content, target context, configuration,
selection and budget. Applying that exact preview persists an attributed
transfer, compact transcript metadata and a local payload snapshot atomically.
No provider is contacted by preview or apply. The next explicit destination
turn includes the captured text; native correlation confirms its delivery.

Provider handoff creates a fresh binding using the existing context-boundary
path. A portable transfer to an unstarted ordinary/fork chat uses that chat's
own binding. Fork-result merge-back keeps the parent's native binding and
copies only source items not already delivered into that target context.
Inherited child chats remain fresh by default; this operation cannot smuggle
context into an agent-created child. Every transfer is conversation context,
never a Git operation or additional authorization from the source text.

Budget: use a matching target's reported context capacity/usage when available;
otherwise use a conservative explicit unknown-capacity allowance. Reserve space
for instructions, the next prompt and response. Estimate tokens conservatively
from UTF-8 bytes and label the estimate. Never silently manufacture a summary.
The next send rechecks the combined transfer/prompt budget. Oversized history
is represented by included/omitted IDs and a paged visible-text retrieval API.
Captured included text survives source deletion; missing or rewritten omitted
source material is explicitly unavailable. Target compaction/rewind/binding
changes invalidate delta cursors and stale previews.

Local transfer records share the snapshot transaction used by commands and
history recovery. Full payloads are local. Shared context state carries bounded
receipt metadata. An attempted delivery is marked unknown before provider I/O;
the same native-correlation transaction that finalizes a send records delivery.
Unknown acceptance blocks reinjection. Existing acceptance-aware recovery stays
authoritative. Clear context can explicitly discard pending transfers while
retaining their audit records.

## Failure matrix (before implementation)

1. A client supplies invented text, hidden reasoning, tool results, approval
   payloads, system instructions, binary attachments or duplicate IDs.
2. Read-only preview starts a provider, writes a receipt or creates a chat.
3. A preview becomes stale after source rewrite/deletion, a parent message,
   model/account/permission change, compaction, rewind or binding replacement.
4. Two apply clicks, two command IDs for one preview, or restart cause duplicate
   injection. Reusing a command ID with a different payload stays rejected.
5. An oversized Unicode message splits a character, exceeds the byte/token
   budget, or gets silently summarized. Unknown capacity is called verified.
6. The current prompt or several pending transfers exhaust the available budget.
7. Applying a transfer starts or interrupts the parent's current turn; native
   steering silently bypasses context waiting for the next explicit turn.
8. Provider acceptance is lost after dispatch. A crash before/after native
   confirmation replays the transfer or prematurely marks it consumed.
9. Destination deletion, source deletion, remote ownership or a source provider
   switch invalidates another surviving chat or grants cross-host authority.
10. Repeated merge-back includes the same delivered delta again; target
    compaction falsely claims the old context is still present.
11. Transfer intent/receipt/payload commit partially after a SQLite failure.
12. A native binding ID leaks into UI, the lineage source changes, or a transfer
    mutates Git refs, files, worktree identity or the user's index.
13. Old engines/clients cannot decode additive fields or lose the existing
    bounded handoff path. Old handoffs retain their original explicit semantics.
14. Portable context changes an agent-created child's fresh-context contract.
15. Omitted-item retrieval returns changed or non-visible content, crosses the
    owning host, or falsely promises retrieval after deletion.
16. A warm provider reports its old session again while retiring after a fresh
    binding was committed. That late completion must not restore a legacy
    resume ID for the new provider or undo an explicit context reset.

Verification is through real RPC/document/storage/provider boundaries and the
production Shell. Retain red/green E2E results, receipt snapshots, exact provider
request counts, native A→B→A and fork-marker recall, restart evidence, and Git
before/after hashes under `target/qa/harness-evolution/slice-2/`.
