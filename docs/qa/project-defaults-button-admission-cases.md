# Defaults rendered-choice admission — failure-first plan and source trace

Status: source-derived concern; no Defaults rendered-choice reproduction has run
and no admission fix is authorized or implemented by this document. Existing
17-case result-isolation/picker proof remains tied to `04da0cf3`. Exact queued
Commands baseline source `0ef61afda7a4b9c173eafac642f906cdbb26ae84` remains an
immutable Git object; this preparation changes only documentation.

Commands' corrected window fixture currently has one passing fresh control and
four failed retained-frame premises. That does **not** validate stale mouse
admission or establish a behavioral bug. Defaults test code waits until the
actual-window retained-frame pattern is validated and any shared test helper is
agreed with its Commands owner. Do not copy or modify that owner's scaffolding
independently, and do not weaken render/bounds assertions to produce a failure.

## Current source path at 0ef61afd

1. `crates/ui/src/settings/project_defaults.rs:360` constructs each settings
   choice from an `Action`. The click closure at line 389 retains the action,
   but no rendered project/owner/engine identity, and calls `page.action(...)`.
   The Enter/Space handler at lines 391–394 reaches the same action method.
2. `action()` at line 254 checks the **live** busy flag. For settings mutations,
   it reads `self.selected(cx)` at line 269, clones that project's current
   defaults at line 282, applies the selected group, and calls `save()` at 309.
   The project-selection branch changes the page's selected project and retires
   request state; it does not bind already-painted choice listeners to the old
   project.
3. `save()` at line 312 refreshes/captures the **current** request scope and
   engine. The RPC at lines 325–327 uses the currently selected project/owner
   and its `expectedDefaults` snapshot. If a click painted for A is admitted
   after B becomes current, this path could construct a valid B write.
4. The accepted callback fence at lines 329–345 checks that the reply still
   belongs to the scope captured at request creation. It correctly rejects
   retired replies, but cannot recover an event's rendered A origin after
   request creation has already treated the event as a B action. Backend owner
   and CAS validation likewise validate that B request, not the visual origin.

This is a source trace of a potential admission path, not a claim that the
required stale-frame input has been delivered in a test or native app.

## Mouse and keyboard have different dispatch rules

The checked-in GPUI `window.rs:5182` mouse path hit-tests and invokes
`rendered_frame` listeners without first forcing a dirty redraw. Its
`dispatch_key_event` at line 5240 explicitly draws when dirty before dispatch.
Therefore a stale mouse result must not be generalized to keyboard input.
Record actual paint, focus and dispatch behavior for a keyboard control; a
framework redraw is expected there and is not evidence of a stale callback.

## Bounded suite to write after fixture validation

Use the real ProjectsPage rendering, actual `Window::dispatch_event` input,
current-thread Tokio memory RPC, and actual engine-attachment/selection paths.
Two synthetic projects A/B should both permit the tested setting so a backend
validation error cannot accidentally make wrong-target admission appear safe.
Use one representative mutating choice (for example New worktree) from the
shared `choice()` handler, with distinct A/B defaults and a deliberate composer
draft. Do not run providers or shells.

| Case | Transition and actual input | Required invariant / control |
| --- | --- | --- |
| DA1 — project replacement | Paint an enabled choice for A; select B through the existing production project-selection path; deliver a mouse click to the genuinely retained A frame. | No save is sent to either A or B; B's defaults, current selection and deliberate draft remain unchanged. The fixture must prove no intervening repaint before input. |
| DA2 — same-device engine generation | Paint A, attach engine B with the same project ID and device but a newer generation, then click the retained A choice. | No B save; generation really changed, rendered origin remains A, and B project/page/draft state is unchanged. |
| DA3 — different-device engine | Paint A, attach B with the same project ID but a different engine/project owner, then click the retained A choice. | No old-origin save reaches B or is rerouted to A. Verify both engine identity and project owner changed. |
| DA4 — fresh current choice | Render the current B page and activate its actual choice once. | Exactly one B save with the correct project/owner and current `expectedDefaults`; the intended group changes while unrelated defaults and the composer draft remain intact. |
| DA5 — same-project watch/keyboard control | Update a nonidentity field such as project display name while project/owner/engine generation remain stable; use the real focus/keyboard path on a valid current choice. | The control remains usable and saves once. Record any mandatory key-dispatch redraw; do not require the old mouse-frame premise here. Preserve updated metadata, unrelated defaults and draft. |
| DA6 — existing live-busy control | Paint an enabled choice, establish a pending save through the production action path, then click the old enabled frame while the live page is busy. | The existing live busy guard prevents a second save. After the first held request settles, the page remains usable. Do not set the busy flag directly or remove the original busy check. |

The three stale-context cases are expected-failure hypotheses only. The three
controls are expected-pass hypotheses only. Actual execution counts/outcomes
must be recorded independently. If real focus or retained-frame premises cannot
be established, classify that as a fixture limitation, not product proof.

## Evidence and correction boundary

Before target input, record the actual painted choice bounds/point, rendered
project/owner/engine identity, surface render count, live identity/busy state,
A/B defaults and deliberate draft. Record render counts immediately around
input, before any observation helper can trigger another paint. Use cached
frame bounds through a validated path; do not silently redraw while checking
whether the old frame survived. Keep pointer and keyboard trace semantics
separate.

After input, capture real RPC method/params/engine/settlement, current page and
project state, and the draft. A valid stale-context baseline must reach the
behavioral assertion with all frame premises satisfied. A passing fresh control
must show one real request rather than merely a local state change. Preserve
`expectedDefaults` in the request and the existing engine CAS/owner regression
proof; no unconditional writes or hidden global-default updates are allowed.

No production admission guard is to be added before authentic failure-first
proof and the coordinator's next source scope. Any eventual guard must preserve
valid current choices, same-project metadata updates, the existing live busy
check, explicit draft choices, and backend CAS behavior. Capture immutable
baseline/fixed source, exact commands, exits, case counts and JSON separately.
No new Cargo, native interaction, main/schema integration or broadened test
scope is authorized by this plan.
