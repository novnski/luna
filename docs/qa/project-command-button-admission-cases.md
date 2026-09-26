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

## First compile/run: fixture borrowing error

Combined `896106a1` compiled but all five cases panicked before producing traces
at GPUI entity_map.rs142: ButtonSurface was already being updated. This is zero
authenticated admission failures. Original source `2b9c4f2c` and Defaults artifact
`target/qa/project-defaults/button-admission-baseline-1/tests.log` are preserved.

Cause: the fixture called window.draw inside typed
WindowHandle<ButtonSurface>::update, which already held a mutable root-view lease;
draw then tried to update the same root for rendering. The test-only correction
uses VisualTestContext::update (Window/App only) for explicit draw and mouse
dispatch. Real rendered buttons, old-frame input timing, render/bounds provenance
and every behavior assertion remain unchanged. No admission fix is included;
the corrected fixture still requires an authorized baseline run.

## Second baseline: eager test-frame painting invalidated the premise

Combined `0ef61afd` produced five traces: the fresh control passed; three stale
cases found their old bounds removed, and the busy case saw render count2→4.
All requests prohibited by these cases remained zero, but their frame premises
failed, so this is still zero authenticated admission defects. Evidence remains
in Defaults' `button-admission-baseline-2/` directory.

The source cause is GPUI app.rs flush_effects: under test/test-support/bench only,
its end-of-update loop synchronously draws dirty windows. The fixture used one
update for attach/busy and another for reading bounds/input, so that test-only
paint replaced the old callbacks first. Production mouse dispatch itself does
not perform the keyboard path's dirty redraw.

The corrected fixture keeps the real attach (or real busy-start operation) and
real Window mouse dispatch inside one raw Window/App update. It captures scope,
live busy and render counts inside that region, before the eager test-support
paint at update completion; post-update render count is recorded separately.
Initial painted bounds and the same no-intervening-render/behavior assertions
remain. No GPUI source, production guard, helper callback or runtime behavior is
bypassed/modified; no direct call substitutes for the tested mouse callback.
Baseline verification remains pending a separate grant.

The coordinator reviewed and accepted the single-boundary direction before its
commit. Delivery JSON is captured inside the closure: live AppState generation/
device/chat and the Commands child's cached scope are distinct fields; an
observer need not already have processed replacement. Painted bounds come from
the real button's debug-bound query before entry, and unchanged render counters
are asserted through actual MouseDown/Up delivery, establishing those bounds are
still the delivered frame. Scope/busy/render observations before and after input
are captured inside the same closure. Post-update eager drawing is recorded in
a separate afterUpdateRender field and is not a stale-input premise.

## Authentic baseline and minimal admission correction

Coordinator and Commands owner read the exact `80bd8e08` baseline3 artifacts:
fresh control1pass; four authenticated behavior failures. Stale Run/Retry/Stop
each sent one RPC toB with old bounds retained and painted/before/after-input
render counts2/2/2; the test-only post-update count was3. The busy case also sent
one target Build RPC with busy=true and input render2. Actual live generation2
and cached/painted generation1 were separately recorded. Logs and JSON remain
in Defaults' `button-admission-baseline-3/`; earlier failed fixture evidence stays.

Only after that baseline, the production source correction captures each Run
and Retry/Stop button's rendered scope and checks it against live AppState before
entering execute. execute rechecks live busy after synchronizing its current
scope and returns without replacing an existing waiter when busy. The existing
response/output fences, PTY ownership, backend receipts, polling cadence and UI
layout/theme are unchanged. No fixture/assertion/helper/vendor changes or new
tests accompany the correction. Fixed verification is pending an explicit grant.
