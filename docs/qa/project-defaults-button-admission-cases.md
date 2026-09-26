# Defaults rendered-choice admission — failure-first plan and source trace

Status: source-derived concern; no Defaults rendered-choice reproduction has run
and no admission fix is authorized or implemented by this document. Existing
17-case result-isolation/picker proof remains tied to `04da0cf3`. Exact queued
Commands baseline source `0ef61afda7a4b9c173eafac642f906cdbb26ae84` remains an
immutable Git object; this preparation changes only documentation.

Commands baseline-3 on `80bd8e08` now validates the single raw Window/App
boundary: one fresh control passed, and four cases reached behavioral admission
failures with retained bounds and unchanged delivery render counters. Earlier
fixture failures remain preserved separately. Defaults still has no admission
reproduction of its own. Its test code can now be written after agreeing minimal
window-helper reuse with the Commands owner. Do not modify that owner's frozen
assertions or shared scaffolding independently.

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


## Source-only reproduction checkpoint

The six Defaults cases are authored in
`crates/ui/src/settings/project_defaults_button_admission_tests.rs`, with a
cfg(test) module inclusion and one test-gated New worktree debug selector.
No Defaults runtime admission guard has been added. Commands owner agreed that
a tiny page-specific root/paint wrapper and the two raw mouse dispatch calls
are preferable to changing its validated fixture. Existing HeldRpc, attachment,
pump and trace helpers are reused unchanged.

The mouse cases measure actual page scope and render counts around input inside
one raw Window/App update. Engine changes use the real attach path; project
selection uses the production page selector path. A legitimate device change
may reset the prior draft, so the deliberate current draft is hydrated after
transition effects settle but before any held save acknowledges. Wrong-target
dispatch is asserted independently; the current draft must remain unchanged
across the held completion.

The keyboard control uses a real mouse activation to focus New worktree,
settles that setup save, applies a same-project metadata projection via
`apply_spaces`, then uses actual previous-tab-stop focus navigation and Enter
KeyDown/KeyUp to select Current checkout. Its measured keyboard activation must
add exactly one request with the latest expected-default snapshot. Keyboard
redraw behavior is recorded rather than requiring a stale-mouse premise.
Fresh and busy controls also inspect request targets/CAS fields and the draft.

Direct rustfmt and diff checks passed. No Cargo, fixture execution, test pass or
behavioral Defaults admission failure is claimed at this source checkpoint.

## Authentic baseline and minimal source correction

On exact clean `ff7f5620`, the granted six-case run compiled and produced
**3 controls passed / 3 stale-choice failures**, exit 101. Every retained-bounds,
render-count and scope-transition premise passed before the three wrong-save
assertions. Each stale project/same-device-generation/different-device input
sent one save through the new live context. Fresh target/CAS/draft, live-busy
first-waiter preservation, and same-project metadata plus real keyboard controls
passed. No fixture or compiler error was classified as a product failure.

The full baseline packet is immutable under
`target/qa/project-defaults/independent-baseline-1/defaults-admission`; sibling
Move and Commands results are separate. `classification.json` and `sha256.json`
at the packet root record group outcomes and raw evidence hashes. The UUID
MoveProject control passed; no materialization engine companion ran.

After coordinator acceptance, the Defaults-only source correction captures
rendered project/owner/engine scope for each choice and checks it in both mouse
and Enter/Space callbacks before invoking the live action method. Engine
generation is retained even with no selected project, so a stale project-selector
frame cannot cross a connection replacement. The existing live busy guard,
CAS payload, draft behavior, keyboard event consumption and all six/fixed-22
assertions remain unchanged. Direct rustfmt/diff checks passed; fixed execution
and native acceptance are still awaiting their own grants.
