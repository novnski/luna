# Empty-project moon native acceptance

## Recording analysis and design direction

The supplied recording is 20.49 seconds long. Its first flourish occupies
approximately 2–9 seconds; the second occupies approximately 11–18 seconds.
Both begin with a dim, readable emblem, brighten during motion, and return to
the original dim pose. The pointer has already left during the first turn, so
the effect behaves as a triggered sequence rather than requiring sustained
hover. These timings are observations from sampled frames, not source-level
claims about Codex's implementation.

Three visual cues create the apparent volume. The emblem narrows when viewed
edge-on, its internal parts overlap differently as it turns, and its shading
changes with orientation. Around 4–5.5 seconds, a terminal `>_` symbol faces
forward; the knot returns near the end. That reverse face connects the playful
detail to the tool's purpose. A fixed character grid and stable center keep
the surrounding terminal layout still while the object changes. The recording
alone cannot establish whether Codex uses geometry, precomputed frames, or
another renderer.

LUNA's previous view already had a useful moon identity and generous negative
space. Its 448-pixel text illustration was static, with one foreground color
across the rows. The dense full-disc texture supplied surface detail but weak
directional lighting. The heading, explanatory subtitle, and button all served
the same onboarding instruction, so the copy added visual weight without three
distinct pieces of information.

The chosen direction is a quiet lunar observatory: craters, phases, orbits,
regolith, and earthshine inform the artwork. Charcoal, basalt gray, regolith gray,
silver, and pale highlights appear through existing theme colors and glyph
density. Its signature is an actual text moon whose surface travels around a
lit sphere. A sphere stays circular during rotation; flattening the existing
image into an edge-on plane would imply a disc instead. The design avoids a
perpetually spinning ornament, extra decorative particles, and repeated setup
instructions.

The moon's maximum size is now 320 pixels, about 29% smaller. The upper-left
light stays fixed while surface features turn once. Smooth acceleration and
deceleration return it to the exact original surface, with no idle animation
loop. The headline invites the user with `What will you build?`; `Add project`
owns the action, so the subtitle can be removed. The existing project picker
continues to explain device and folder selection at the moment those choices
are needed. This retains the product's typography, themes, and workflow.

## Repeatable acceptance

Exercise these failure cases against the native empty-project fixture before
accepting the welcome screen. Use isolated fixture data and one LUNA GUI process
at a time; preserve the primary application's data and build cache.

1. The empty-project screen must show `What will you build?` and one `Add project`
   action. The old instructional heading and subtitle must be absent.
2. The moon must remain circular and visibly lit from the upper left, with
   surface features moving around the sphere rather than a flat image turning.
3. A pointer entering the moon starts one complete turn. Leaving must not snap
   the surface back; repeated entry during that turn must not restart it. The
   final frame must match the original resting frame.
4. An idle screen and a completed turn must stop requesting animation frames.
   Opening another route must not leave a hidden animation loop running.
5. Reduced motion must keep the moon still, including when enabled during a
   turn. Light and dark themes must both retain the moon's lighting and readable
   copy.
6. The moon, heading and button must fit the smallest supported window without
   clipping. Opening `Add project` must still reach the existing project picker.

Repeat the native headless acceptance with:

```sh
scripts/cargo.sh build --locked -p luna-ui --example empty-project-fixture --features appshots-fixture
target/debug/examples/empty-project-fixture target/qa/empty-project-moon-2026-09-30
```

This renders the production Shell with the macOS backend in hidden windows;
it can run while the primary app stays open. Passing no output argument launches
the interactive fixture instead. Gracefully quit the exact primary LUNA app
before that interactive mode. Set `LUNA_PALETTE_LIGHT=1` for its light appearance.
No account, provider or primary application data is used.

Acceptance on 2026-09-30 passed all 13 checks. Evidence is retained under
`target/qa/empty-project-moon-2026-09-30/`: `result.json`, resting/turning/settled
captures, reduced-motion captures, light appearance at 1100×800 and 700×560,
the project picker and route exit. `SHA256SUMS` records the evidence hashes.
The synthetic fixture's device has no engine connection: the picker-open check
verifies the existing action is reached, not folder browsing or project creation.
Headless frames are explicitly rendered at checkpoints. The refreshed primary
application is verified by its exact process, IPC listener and strict signature.
The user exercised its live animation and requested a slower turn; the base
duration is now 4.8 seconds, one third slower than the initial 3.2-second turn.
Frame rate was not benchmarked.

Required checks: `scripts/cargo.sh check --locked -p luna-ui`, native acceptance,
`scripts/cargo.sh clippy --locked -p luna-ui --lib --example empty-project-fixture
--features appshots-fixture`, and `scripts/cargo.sh fmt --all -- --check`.
