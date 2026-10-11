# SW-05 transcript and identity verification

Source: `4128f083`, 11 October 2026. Providers: Codex `0.160.0` (`gpt-6.1-sol`)
and Claude Code `2.1.293` (`claude-opus-5-5`), both in Full mode, on macOS 27.0.1.

**Result:** no transcript, ordering, identity or viewport defect reproduces on
current `main`, so no runtime code changed. Per the ticket, a passing baseline
closes as verification. The [failure cases](failures.md) were written before
the live runs.

## Deterministic owners, rerun green

| Package | Suites | Result |
| --- | --- | --- |
| `luna-engine` | `timeline_order_e2e`, `codex_subagents`, `claude_native_agents`, `claude_native_tasks`, `tool_output`, `transcript_salvage` | 14 passed |
| `luna-harness` | `claude_tool_results`, `opencode` | 19 passed, 1 live-gated ignored |
| `luna-doc` | all | 98 passed |
| `luna-ui` | `transcript::` lib tests (viewport, anchor, runway) | 81 passed |

[Summary](evidence/owner-suites-summary.txt). These suites still own fold and
part identity (timeline cases 1–10), child late results after a steer, parked
native updates, and the viewport-cache logic.

## Gap analysis

- **Adapter boundary.** `timeline_order_e2e` injects provider events through a
  test harness. The real adapters each emit their own message boundary and test
  it: Claude per `assistant` frame, Codex per completed message item, OpenCode
  per message. So the fold and the adapters are owned separately, with no
  uncovered seam.
- **Claude late child result after a steer.** The fix for the Codex version
  lives in the shared engine `SubagentSink.fold`, and Claude's tagged child
  events go through that same path. It is covered, so no new test was added.
- **Native viewport.** The ticket needed this proven live. Before this run, no
  native path read the viewport (`scroll_readback()` has no callers), so the
  check uses real captures.

## Live native run (E06)

The disposable QA app was `LUNA QA Transcript` (`com.novnski.luna.qa.transcript`,
port 59851, isolated data, MCP isolation on), driven by
`scripts/qa-opencode-live.mjs`. Each provider ran two turns:

1. An ordering turn: text → shell → text twice, then a LUNA `create_thread`
   helper whose result lands mid-turn during a 12-second command. Claude also
   launched a native Agent subagent.
2. A viewport turn: 120 streamed lines with two 20-second commands.

The [prompts](evidence/prompts/) are the replay inputs.

| Case | Codex | Claude | Evidence |
| --- | --- | --- | --- |
| N1–N3 chips once, native order, no glued messages | pass | pass | [codex](evidence/codex-transcript-fidelity.json), [claude](evidence/claude-transcript-fidelity.json) |
| N4 helper "created" card and late result in chronological position | pass | pass | Card write time checked against the provider record: Codex 22.589 after "Step four" 22.501; Claude 14.592 after `create_thread` returned 14.575, before the Agent `tool_use` 15.371 |
| N5–N6 resolved chips, finished entries, unique part IDs | pass | pass | fidelity reports |
| N7 native subagent stays in its own transcript | — | pass | Agent chip has `subagentRef`, status `done`, 1 Bash call; none in the root |
| N8 content identical across a graceful restart | pass | pass | Transcript hash unchanged (`d0698c59…`, `2f94e8e9…`); driver `verify-restart`: new PID, each send once, no replayed queue row |
| V1 mid-history reader still while the turn streams | pass | pass | 0 changed pixels across 4–5 frames over 21–32 s ([diffs](evidence/viewport-diffs.json)) |
| V2 not pulled to the tail at turn end | pass | pass | 0 changed pixels while the composer turned stop → send |
| V3 position kept across a chat switch | pass | pass | 0 changed pixels Main → helper tab → Main |
| V4 first visible row kept through a resize | pass | pass | [frames](evidence/frames/): shrink (Codex), Window > Fill (Claude) |

Positive controls rule out stale or blind captures. In the same frames, the
composer, the scroll itself and the tab switch each changed 19–39 mean levels,
so the windows were rendering live while the reader's region stayed exactly
still. A mutation check showed the checker fails the intended case each time:
glued text fails N3, a duplicated chip fails N1/N2, and an altered restart
snapshot fails N8.

Codex 0.160.0 runs tools through a JavaScript code-mode `exec`. LUNA shows one
chip per inner shell or MCP call. A pure-JS helper call (tool discovery) has no
inner item and gets no chip. A `write_stdin` poll folds into its session's
chip. Both are expected.

## Repeat

```sh
python3 scripts/qa-transcript-fidelity.py <driver-run-dir> codex|claude-code <native-transcript.jsonl>
```

Run it after the driver's `run`, `verify-restart --provider …` and
`snapshot --provider …`. [commands.json](evidence/commands.json) has every
command and exit code. [sha256.json](evidence/sha256.json) hashes the committed
evidence, the local raw run
(`target/qa/luna-only-workflows/sw05-20261011T160312Z/`) and the native provider
transcripts, which stay local and are not committed.

**Limitations:**
- Full mode only. Ask and Auto change permissions, not the transcript fold.
- One real run per provider. Model output varies, and the fidelity checker
  compares against the provider's own record rather than a fixed expectation.
- OpenCode stays with its own OC campaign.
- Viewport position across an app restart is not restored by design, since the
  cache is in memory. N8 covers content across a restart instead.
- V4 is checked from the visible first row, not from pixels, because the text
  reflows.
