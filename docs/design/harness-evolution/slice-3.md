# Shared background work

The host projects existing child chats, native child chips, native coordination
tasks, and checkout commands into one bounded view. It does not create another
executor. Explicit launch ownership is separate from chat ancestry. The initial
delegated message inherits its launching task's ownership; a later independent
message belongs to that child itself. Grandchildren retain their original owner.
Persistent checkout commands are services and do not hold task completion open.

## Failure matrix (before implementation)

1. A parent turn ends while its child or grandchild still works: retain blocking
   work, even when the intermediate child's own turn has completed.
2. A child is reused independently, including on a warm native session: stopping
   the earlier delegation must not cancel that later message.
3. A legacy child or fork has ancestry but no explicit launch ownership: never
   infer stop authority from the parent/source link.
4. A dev server remains running: show it as a service, excluded from owned-work
   stopping and completion accounting.
5. A stop preview becomes stale through a new message, binding, host, deletion,
   or terminal event: refuse that exact target; never retarget to current work.
6. A native stop is acknowledged without terminal evidence: show requested,
   not stopped. A lost answer/crash is unknown and must never replay the stop.
7. A duplicate stop request uses the same ID with another preview: conflict;
   identical retries return the durable receipt without provider I/O.
8. An adapter cannot address one child/workflow/shell: show unavailable with a
   reason, never substitute stopping its parent or killing by process name.
9. A reconnect/restart has historical active native work without live evidence:
   show unknown. Keep terminal evidence and pending result delivery visible.
10. A parent is archived/offline or has a held result continuation: distinguish
    outcome publication, queued continuation, and confirmed provider delivery.
    Keep existing stable outcome IDs and peer-origin permission envelopes.
11. Native child and workflow observations overlap: stable kind-specific IDs
    and strict subagent classification prevent duplicate/misclassified rows.
12. Two panes watch work: only the owning pane opens or stops its selected work;
    stale watch responses cannot replace a newly focused task's state.
13. Large histories/child sets: bounded pages, explicit truncation, deterministic
    ordering; a partial page never becomes an implicit stop-all selection.
14. SQLite receipt persistence fails: dispatch no stop before the unknown intent
    is durable. A completion write failure keeps unknown, not automatic retry.

Verification uses RPC/SQLite/protocol-peer E2Es and the production workbench
fixture, followed by native Codex/Claude evidence in a disposable profile.

## Contract

`GetTaskWork` / `WatchTaskWork` take `{taskId, offset?, limit?}`. Only the owning
host answers. Work IDs are application digests; native session/task IDs stay
private. A page includes aggregate blocking/service/unknown counts and the next
offset. `PreviewStopTaskWork` takes `{taskId, workIds}` and returns the exact
eligible targets plus exclusions and a hash-bound preview. `StopTaskWork` takes
`{taskId, workIds, previewId, requestId}`; its receipt is durable and repeatable.
The ordinary Stop button still stops the current turn. Services use their
existing command controls. Stop owned work never includes ancestry-only rows.

Additional observed shell boundary: Claude's typed `local_bash` task frames
currently disappear for root jobs after the launching tool returns. Preserve
those native lifecycle observations in the existing native ledger, while still
routing a nested shell's terminal tool result to its actual child transcript.
Never classify a shell as a subagent, consume nested completion with the new
observer, or count the launching shell chip and its native task twice. Native
shell stop remains unavailable until the adapter exposes an exact control.

Shell lifecycle uses additive `meta.nativeShellTasks`; the existing
`meta.nativeTasks` and `WatchNativeTasks` workflow view keep their old shape.
The shared work view combines both, so an older client can still decode its
workflow ledger. Both ledgers settle on provider exit.
