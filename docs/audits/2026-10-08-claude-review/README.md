# Recent LUNA feature review and native QA

The review covers **185 commits across 29 feature families**, including **169
Claude/Anthropic coauthored commits** and 16 shared foundations in
`bd603de1^..d3819203`, plus the earlier parity dependency `010bbadf`. Eight
GPT-6 Astra reviewers inspected separate areas; the lead reviewed their patches,
ran the checks and real-provider scenarios, and performed native computer-use QA.

The detailed [coverage ledger](coverage.md) and [machine-readable inventory](coverage.json)
distinguish source review, deterministic tests, native fixtures, signed-app RPC,
and observed mouse/keyboard behavior. A passing scenario proves its recorded
assertions, not every possible timing, account, remote machine or provider release.

Final evidence includes **1,767 passing Rust tests across 59 targets** (10 existing
ignored tests), **21 real-provider signed-app scenario passes**, **four controlled
fault/compaction app passes**, **19 paid OpenCode Go probes**, and native UI,
scheduled-run, context-transfer, permission-transition and restart checks. The
Settings gallery has 168 captures; startup has 26 passing runs; both 1,200 and
10,000-entry performance runs pass all 44 checks. Precise counts and limitations
are in the ledger.

## Delivered fixes

| Commit | Result |
| --- | --- |
| `916df049` | Schema-18 backups work and retain automation definitions/history. |
| `a6a4a530` | Claude resume/history operations respect the selected provider home and live session owner; history helpers isolate identity and hooks. |
| `0e37d858` | Carried questions retain deadlines and holds; late async cards survive turn boundaries; archive/child status and restart holds are accurate; surviving forks retain workspace discovery; split/steered history checkpoints remain usable; command dispatch no longer overflows the default stack during goal resume. |
| `8517ae33` | OpenCode bounds empty-stream reconnects, reconciles recovered text and requests, recovers parked child completion, and records a terminal provider failure once. |
| `dd80eb3d` | Question and amendment drafts survive chat navigation; Hold failures remain visible; multiline answers grow then scroll; Settings catalogs refresh; startup handles Ready-before-Shell ordering; task/card controls remain accessible through redraws. |

Each commit was pushed to `origin/main` over SSH and checked against remote HEAD.
The native performance fixture was also repaired to distinguish byte/count cache
limits and retain a bounded positive-cache probe. Production cache ceilings were
not changed. Runner fixes preserve failed evidence and strict app isolation.

## Evidence and reproduction

The full local evidence directory is
`target/qa/claude-review-20261008/`. It contains logs, command manifests, provider
wire slices, screenshots, AX trees, native readbacks and file hashes. Individual
reports explain failure-first cases, changes and their acceptance boundaries:

- [Inputs, queues and command dispatch](engine-inputs.md)
- [Workspace, children and background work](workspace-engine.md)
- [History, backup and native QA runners](history-platform.md)
- [Claude, Codex and external-writer protection](claude-codex.md)
- [OpenCode recovery](opencode.md) and [installed OpenCode Go probes](opencode-live-go.md)
- [Composer, workbench and accessibility](ui-chat.md)
- [Settings and usage](settings-usage.md)
- [Native failure and acceptance cases](native-cases.md)

All Rust verification uses `scripts/cargo.sh --locked` in the existing checkout
and target directory. Native app scenarios use the signed **LUNA QA Review**
identity on port **59841**, a separate data directory and MCP isolation. The
saved `qa-env.sh` supplies all four identity values; the runners reject a partial
or primary-app identity. The expanded native runners use
`LUNA_AUDIT_NATIVE_BASE` to keep their artifacts separate while sharing that app.

The 19 OpenCode Go probes use the installed **2.0.24** CLI and an existing
configured provider profile, with current-checkout binaries. Credentials were
not copied. Signed-app OpenCode runs use the new isolated profile and explicitly
record their own model. These are separate evidence sources.

## Interpretation of failed attempts

Earlier failed attempts remain in the evidence directory. They are not relabeled
as passes. The ledger links corrected reruns where applicable:

- The first OpenCode free-model S9Q hit provider quota; a later MiMo run passed
  the actual question, amendment, repeat and steering assertions.
- Blocking-question prompts originally allowed LUNA's nonblocking question tool;
  the corrected scenarios explicitly request the native blocking tool.
- A typed-skill test supplied an empty path; LUNA correctly rejected it. The
  corrected runner sends the real fixture's absolute `SKILL.md` path.
- A 6,000-token goal budget stopped after the first continuation. The finite,
  configurable 60,000-token rerun completed both autonomous steps and preserved
  their history checkpoints.
- An automation test read its transcript immediately after the independent run
  ledger completed. The corrected test waits for the exact completed exchange
  without replaying any occurrence.
- A background/occluded Welcome screenshot did not prove a startup deadlock.
  Raising the signed empty-profile app showed the expected ready Welcome view.

Claude workflow feature work remains intentionally deferred. Account-gated
remote/cloud features, multi-device sync, publishing a release/updater and
exhaustive supported-version certification are outside this local acceptance
run. OpenCode 2.0.24 retains its existing product compatibility classification;
these results do not silently certify other versions or every provider contract.

No additional Rust checkout or worktree was created. The pre-existing
`docs/pending-tasks/agent-workspace-app-research/` content is unrelated and remains
untouched. Final verification, artifact hashes, cleanup and the primary LUNA Dev
handoff are recorded in the coverage ledger.

LUNA Dev is refreshed and its process, port and strict signature were verified.
The reviewed QA bundle and owned mock servers were removed; QA data and evidence
remain. The 29 disposable scenario directories are preserved in
`disposable-scenarios.tar.gz` with checked file hashes before scoped removal.
