# Commands button admission: failure-first cases

Source-only review of `40b0720c`; the response/output fences remain intact.
No outbound admission guard or new build is part of this preparation.

The Run and Retry/Stop closures capture action/run IDs from their rendered
snapshot. `execute()` first syncs to current scope, then selects the current
engine without receiving the rendered origin or checking live busy. GPUI mouse
dispatch uses rendered_frame.mouse_listeners without the dirty redraw performed
by keyboard dispatch (`window.rs:5181` vs `:5233`). Div's click handlers persist
from paint until the next frame. An old enabled mouse callback therefore remains
invokable before redraw after an engine replacement or busy-state change.

Backend ownership/checkout checks reject mismatched or unknown targets, but do
not encode the origin of the UI callback. Run resolves the action ID in the
current engine. A same-device new runtime retains project/action/receipt IDs, so
those checks do not categorically exclude a stale UI dispatch. Run request IDs
are minted anew on every invocation; they do not deduplicate two UI admissions.

Prewritten actual rendered-button GPUI/RPC cases:

1. Paint Run onA, attachB while preserving task/action IDs, deliver mouse down/up
   against the last painted button before redraw. B must receive no Run request.
2. Repeat for a painted Retry receipt and a painted Stop receipt. B must receive
   neither operation from the old frame, even when its IDs remain meaningful.
3. Paint an enabled Run button, begin another execution through the production
   controller (live busy becomes true), then activate the still-painted Build
   button before redraw. The second Build admission must not replace the waiter
   or dispatch a second user action. The first action's cancellation/acceptance
   is not assumed from UI task replacement.
4. Positive control: the same freshly painted eligible button in the same engine
   dispatches exactly once. No state predicate or callback is mirrored; actual
   Window mouse dispatch and the rendered callback run.

Only cfg(test) debug-bound selectors are added to locate the real buttons. RPC
responses are synthetic/held; no shell or provider runs. JSON records painted
bounds, old/current scope, busy state, mouse dispatch and A/B request envelopes.
The existing epoch/assertion suites are unchanged. Baseline execution is pending
an explicit serialized build slot; expected failures are not observed facts yet.
