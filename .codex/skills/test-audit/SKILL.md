---
name: test-audit
description: "Invoke whenever writing, changing, reviewing, or sweeping LUNA tests (Rust, edge TypeScript, iOS Swift, QA scripts). Authoring gate for new tests plus audit workflow for low-value, implementation-coupled, or duplicative tests and the test-only production seams they demand."
---

# Test Audit

Adapted for LUNA from OpenClaw's
[`test-audit` skill](https://github.com/openclaw/openclaw/tree/main/.agents/skills/test-audit).
This is the default way to write and change tests in this repository.

Three modes, one value bar. Authoring mode gates every new or changed test at
write time. Audit mode runs focused sweeps of tests that re-assert source,
duplicate stronger proof, couple behavior to implementation, or keep test-only
production seams alive. Continue broad audits as separate coherent follow-up
commits; optimize for confidence, not deletion count. Campaign mode prunes one
whole subsystem's test surface (every test file a crate, app, or area owns);
before starting one, read [CAMPAIGN.md](CAMPAIGN.md).

## LUNA testing defaults

- Prove behavior at the real boundary first: an integration test in
  `crates/*/tests` that drives the engine, harness, sync, or RPC surface with
  real Loro documents, real Git repositories in temp dirs, and the scripted
  agent peers in `crates/harness/tests/fixtures`; headless UI fixtures; edge
  `workerd` tests; or a native QA script. These end-to-end proofs are the
  preferred and usually sole test for a feature.
- Do not add unit tests after the implementation is written. When a system
  must be tested in isolation, write down every way it can fail first, then
  write the code, then the tests that pin those failure modes.
- End an E2E or QA run with a verifiable, repeatable artifact: the command,
  its exit code, and a retained log, JSON result, or capture directory.
- A pure function with a real input space (parsers, layout math, protocol
  codecs, migrations) may keep a table-driven test at its own boundary; that
  table is its single owner.

## Authoring gate

Before adding any test, answer four questions; a missing answer means do not
add it yet:

1. What observable behavior, invariant, or independent contract does it protect?
2. What credible regression makes it fail?
3. Why does existing coverage not already catch that failure? Each contract has
   one primary test owner at the strongest boundary; another layer needs its
   own distinct risk, such as a transport or lifecycle failure the owner cannot
   reach. Prefer extending a table-driven case or shared fixture over a
   near-duplicate test; consolidate duplicated setup in the same change.
4. Does it need a production seam (`pub` item, `#[cfg(test)]` accessor,
   `cfg(any(test, ...))` export, feature flag, wrapper, injection hook) that no
   production caller needs? If yes, move the test to the real boundary instead.

Then check the test against every [junk pattern](#junk-patterns); a match fails
the gate unless the [retention bar](#retention-bar) names the contract it
independently guards. A test that would break under behavior-preserving
refactoring is asserting implementation, not behavior; rewrite it at the
owning boundary before landing it.

Bug regression tests must fail on the pre-fix code for the intended reason and
pass after the owner-boundary repair. A regression test that never demonstrably
failed proves the mock, not the fix. One regression at the owner boundary
covers the bug; do not replay the same scenario at every layer it crosses.

## Junk patterns

The shared checklist for both modes: the authoring gate rejects a new test that
matches one, and audits hunt for existing tests that do.

- assertion-free coverage probes (a test that only constructs or calls, or
  asserts `true`, `is_ok()` on infallible paths, or a smoke value nothing
  depends on);
- self-comparisons and identity copiers (struct built from literals, then the
  same literals asserted back; serde round-trips of plain derived structs with
  no wire contract);
- copied fixtures, inventories, manifests, or export lists;
- exact source, import, or string greps (`include_str!` of a source file then
  `contains`), unless the retention bar applies;
- private predicate or call-shape tests duplicated at real boundaries;
- duplicate invocations of the same contract;
- crate- or view-local replays of shared helpers already owned elsewhere;
- tests whose only purpose is preserving test-only exports, globals, or wrappers;
- dead production code whose only callers are tests;
- expected values produced by the helper or renderer under test;
- mocks or fake peers that implement the asserted behavior, or one identical
  fake standing in for different providers (Claude, Codex, OpenCode);
- fixtures that supply the receipt, admission, or callback ordering the owner
  should produce, or persistence asserted against a store the path never writes;
- capability tests that restate declared flags instead of exercising the
  delivery or acknowledgement the flag promises;
- negative controls that pass for an unrelated reason, such as a denial from a
  different guard or a rejection the production path never reaches;
- names or fixtures that promise more than the input exercises, such as a
  "retires the window" test asserting the window was not cleared;
- sleeps or timing windows standing in for the event the owner emits.

## Value bar

Tests justify their maintenance cost by protecting behavior, a credible
regression, or an independently meaningful contract. In an audit, an existing
test that must change for behavior-preserving source reorganization is suspect,
not automatically deletable; the authoring gate still rejects new ones.

Before judging a candidate, read the complete test and production owner, its
entry point, callers, callees, sibling implementations, overlapping tests, CI
routing (`.github/workflows/*.yml` names test targets and filters), and
relevant `git log -L`/`git log -S` history. Read root and scoped `AGENTS.md`
files first. When the test claims dependency-backed behavior (Loro, GPUI,
tokio, a CLI protocol), inspect the dependency source or types directly.

## Discovery

Keep discovery read-only and report evidence before editing. For broad scope,
run parallel discovery lanes along production owners:

- engine (`crates/engine/src`, `crates/engine/tests`);
- harness and its scripted peers (`crates/harness`);
- UI (`crates/ui`), split by view family;
- sync, documents, protocol, and RPC (`crates/sync`, `crates/doc`,
  `crates/proto`, `crates/rpc`);
- small crates and apps (`crates/preview`, `crates/syntax`, `crates/theme`,
  `crates/update`, `apps/luna*`) and the edge worker (`edge/`);
- iOS (`apps/ios/LunaTests`, `apps/ios/LunaUITests`) and QA tooling
  (`scripts/qa-*`, `scripts/verify-*`, CI workflows);
- a cross-cutting pattern sweep.

`vendor/` holds upstream forks; audit only LUNA-authored patches there.
Outside campaign mode, prefer a few high-confidence candidates over a large
speculative inventory. Hunt for the [junk patterns](#junk-patterns).

## Retention bar

Keep a test when it independently enforces a public API, wire protocol (RPC,
Loro document schema, provider CLI stream), config, migration, storage,
security, platform, default, prompt-byte, generated cross-language (Rust ⇄ edge
TypeScript ⇄ Swift), package, release, or architecture contract. Also keep:

- call ordering when order is observable behavior;
- regressions with a credible failure mode;
- source inspection when it is the cheapest independent guard: it fails when
  the contract changes (the user-facing key, byte, or path) and survives an
  identifier-only refactor;
- recorded real-provider transcripts (`tests/fixtures/claude/*`,
  `tests/fixtures/codex/*`) that pin a CLI version's wire shape;
- a retained test that fails on the baseline: treat it as a possible product
  bug, reproduce it, and repair the owner rather than deleting it.

Static or slow is not a deletion reason. A test that resembles implementation
may still be the independent contract; prove otherwise before removing it.

## Candidate evidence

Record every field below before editing. A missing field means the candidate is
not ready for deletion:

- exact test name and location;
- what failure it can actually detect;
- non-test callers of the covered production or support seam;
- stronger remaining owner-boundary proof, or why no proof is needed;
- relevant history and the reason the test or seam exists;
- production or test-support deletion unlocked;
- risk and the focused validation command.

## Edit shape

Choose one coherent owner-boundary batch. Delete obsolete test-only exports,
`#[cfg(test)]` accessors, globals, wrappers, and dead production paths instead
of preserving aliases. Move retained regressions to their canonical owners.
Consolidate repeated assertions into one table-driven case or one generic
contract.

Prefer net-negative production LOC. Do not add replacement tests that restate
the same implementation, and do not convert uncertain candidates into cleanup
to increase deletion counts.

## Validation

Never edit source or tests while a Cargo build, `cargo test`, Vitest, or
`xcodebuild` run is reading the same checkout. Follow `AGENTS.md` build rules:
`scripts/cargo.sh` with `--locked`, the shared build lock, and the disk budget.

1. Run the smallest owner and sibling tests:
   - Rust: `scripts/cargo.sh test --locked -p <package> [--lib | --test <target>] [<filter>]`;
     UI as CI runs it: `scripts/cargo.sh test --locked -p luna-ui --lib <filter> -- --test-threads=1`;
     use `CARGO_INCREMENTAL=0` for one-off validation runs.
   - Edge: `npm run typecheck --prefix edge` and `npm test --prefix edge`.
   - iOS: the `xcodebuild test` invocation in `.github/workflows/ui-tests.yml`.
2. For removed source greps or plan assertions, run the executable script,
   fixture, or QA harness that owns the real contract (`scripts/verify-*.sh`,
   `scripts/qa-*`, the browser and preview fixtures).
3. Run `cargo fmt` on changed Rust files, then `git diff --check`.
4. Run `scripts/cargo.sh check --locked -p <package> --tests` for every
   changed package; fix new warnings (unused imports or helpers left behind by
   deleted tests) rather than allowing them. Update `.github/workflows/*.yml`
   when a test target, module path, or filter it names moves or disappears.
5. Inspect `git diff --numstat`; report production/tooling separately from
   tests and test support.
6. After final audit edits, review the whole diff once more against the junk
   patterns and retention bar (Claude Code: `/code-review`).

## Landing and continuation

Commit and push through [commit-and-push](../commit-and-push/SKILL.md) unless
the user asks to stop earlier. Land one coherent batch at a time; after
landing, refresh from current `main` and rerun read-only discovery for the next
high-confidence batch.

## Handoff

Report:

- root cause and removed low-value categories;
- production owner simplifications;
- retained false positives and why they remain valuable;
- focused and full proof actually run, with exit codes and artifact paths;
- production versus test LOC;
- commit and push state;
- named follow-ups.
