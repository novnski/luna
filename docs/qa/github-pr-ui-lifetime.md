# PR UI lifetime failure matrix — before runtime correction

Source baseline: `b6dad9fda9203301e1a13c7941bc0ddb05fd9a7b`.
The backend26-case subprocess acceptance and backup preservation already pass;
this matrix concerns the retained native Changes entity's asynchronous UI state.
No production fence changes precede these reproductions. The Commands owner owns
the shared generic held-RPC helper; this feature owns only PR cases/registration.

## Source evidence

- `changes/pull_requests.rs:120` and `:213` fence completions by panel request
  generation and `git_context` (device, cwd, chat), not engine generation.
- `changes/git_actions.rs:11` does not include engine, Git branch/remote or token.
- `changes/pull_requests.rs:177` clones a cached preview; `:183` selects the
  current engine independently, without checking preview provenance.
- `Changes` stays in `Shell.diffs` until surface close (`shell.rs:3566`, `:3801`).
  The tab key is chat ID (`:2784`). Runtime replacement increments AppState's
  engine generation (`state.rs:1923`, `:2040`) without recreating this entity.
- State observation calls `Changes::sync` (`changes.rs:1785`, `:2327`), while PR
  reset is only in render when the Git context key differs (`:4964`). A hidden
  pane need not render the cleared-selection interval.
- `load_git` replaces its Git preview (`changes/git_actions.rs:57`) but does not
  advance/invalidate PR request provenance on a different same-cwd token.

## Failure-first integration matrix

Each case uses the real `Changes` entity and real in-memory RPC envelopes. The
shared helper holds responses deterministically and records request/reply JSON.
No hosted service, real gh, agent run, main app or user Git repository is involved.
A/B have separate RPC identities; same-device/same-chat/same-cwd values deliberately
exercise the missing engine-generation boundary. Cases must run independently
so one expected failure cannot hide later observations.

| Case | Interleaving | Required invariant |
| --- | --- | --- |
| Preview success/error | Start A preview, attach B without drawing retained Changes, release A | B preview, text, notice and busy state receive no A result |
| Details/checks success/error | Start held A details (check rollup included), attach B with same selection, release A | B details/checks and errors remain B-owned |
| Create success/error | A accepts one create, attach B without drawing, finish A | A completes once; B Create count0; no A PR badge/detail/notice/reconcile state applied to B |
| Cached A preview dispatch | Load A preview, attach B while same selection persists, click real submit | B Create count0 until a fresh B preview has been obtained |
| Same-cwd branch token | Hold A PR read/create, replace Git preview by real load_git with changed branch/token, release A | Old PR reply cannot populate the new branch context |
| Same-cwd remote token | Hold A PR read/create, replace Git preview by real load_git with changed remote/token, release A | Old PR reply cannot populate the new remote context |
| Unchanged Git refresh | Refresh identical Git preview while editing title/body or awaiting PR response | Preserve draft text and valid provenance; do not reset unconditionally |
| Selection change | Hold PR result, select different chat before release | Existing live selection guard rejects stale result |
| Cwd change | Hold PR result, change selected chat cwd before release | Existing live cwd guard rejects stale result |
| Close | Hold PR result, close PR panel before release | Closed panel is not repopulated; accepted create is not replayed |
| Replacement gap | Clear runtime, reattach B and restore selection without a Changes render between | A result stays fenced despite the transient selection being skipped |

For retained-state assertions, the test records complete semantic PR state:
preview identity/operation ID, details/checks, badge, notice, busy/reconcile state,
and title/body/base/head text. It compares the B-context snapshot immediately
before releasing A with the snapshot afterward, rather than asserting one field.
Request/reply/cancellation traces classify the admitted A request. An accepted
CREATE must complete exactly once without cancellation, retry or any B create
call. A retired Preview/Details read may complete or cancel once; either outcome
must leave the retired/replacement UI context unchanged.

## Narrow repair design (not yet applied)

Bind preview, in-flight requests and completed details to engine generation plus
selected device/chat/cwd and Git checkout identity/branch/remote/token. Reconcile
that context on state observation and before submit, independently of rendering.
Invalidate stale preview authority when engine/context changes; preserve the
user's title/body where it is safe to re-review, and require a fresh preview
before submitting through B. Do not automatically move an accepted A operation
to B. Let its host-side operation complete once and discard only its stale UI
completion; original journal reconciliation remains available on A.

Only an actual changed Git snapshot should invalidate the preview. Repeating an
identical Git refresh must preserve draft text and request provenance. Keep
existing request-generation and selection guards, component styles, tokens,
spacing and public backend contracts.

## Baseline status

Source report and matrix authored before test/runtime edits. Shared helper API
and prerequisite commit requested from Commands. No Cargo/native run, production
patch or claim of a runtime reproduction in this preparation checkpoint.

## Immutable baseline source preparation

Commands' generic-only prerequisite `3bc2fbf51e4d31cddd1b30077e69623da0ca7e4e`
was cherry-picked as `e4eacd4f`. It contains only the cfg(test) held-RPC module,
its registration, and a test-only proxy into the real AppState attachment path.
No Commands/Defaults production feature code was imported; helper ownership
remains with Commands.

PR reproductions are in
`crates/ui/src/changes/pull_requests/lifetime_tests.rs`, included only under
cfg(test).22 independent GPUI cases retain the actual Changes entity, invoke
its production PR/Git load and submit handlers, replace the engine via the real
attachment path, and release RPC replies through the shared helper. No window
is rendered; this explicitly covers hidden panes and skipped cleared-selection
frames. Full before/after PR state and A/B traces are written before assertions.
The cached-preview case also verifies that an explicit fresh B preview enables
one B create using B's operation ID while preserving authored title/body.

Exact command after an explicit serialized build grant:

```bash
scripts/verify-github-pr-ui-lifetime.sh
```

Equivalent command:

```bash
LUNA_EPOCH_QA_ARTIFACTS="$PWD/target/qa/github-pr-ui-lifetime-baseline" \
  CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib \
  pr_ui_lifetime_ -- --test-threads=1 --nocapture
```

The script preserves command status (including guard75) and retains test.log plus
per-case JSON traces. Reproduction/runtime results are still unverified; only
Rust formatting, shell syntax and source-diff checks ran during preparation.
No production fences changed. Compiler or fixture setup errors, if any, must be
reported separately from authentic callback/admission assertion failures.


## Coordinator fixture corrections (test-only, before baseline execution)

The original immutable reproduction `515550fe` is retained. Source review found
that its settlement baseline was taken after the transition drained executors.
A Close transition could already cancel the pending RPC, making a subsequent
wait for a newer settlement time out rather than classify that cancellation.
The corrected fixture captures settlement/reply/cancellation counters before
admission and transition, then waits for that request's reply OR cancellation.
It records counter deltas and requires exactly one settlement.

Canceled Preview/Details reads are legitimate when their view/context retires.
The fixture no longer treats them as product failures. It still requires no
replay, no B create, and no stale UI state application. An identical Git refresh
must retain its valid pending read and draft rather than cancel/reset it.
CREATE remains stricter: an already-accepted A create must yield exactly one
reply/completion and zero cancellations, regardless of whether that completion
is success or error; B receives no replay and ignores the retired UI result.

The backend cancellation contract makes this distinction necessary:
`source_control/pull_requests.rs:202` documents that dropping create's future
kills its gh subprocess and leaves a persisted attempt unresolved. The shared
runner enforces `kill_on_drop(true)` (`source_control.rs:825`). A unary UI waiter
is not itself a durable operation owner: `RpcClient::call` sends a request and
awaits a oneshot (`rpc/src/client.rs:141`), while connection/server teardown can
abort in-flight handlers (`rpc/src/server.rs`). Therefore the eventual UI fence
must retire only the view's authority to apply the result; it must not cancel or
reissue a previously accepted create to achieve that retirement.

These corrections modify only the PR test module/report. The generic helper and
all production request/response guards are unchanged. Rustfmt, shell syntax and
source-diff checks ran; Cargo/native/baseline execution remain unrun and gated.
