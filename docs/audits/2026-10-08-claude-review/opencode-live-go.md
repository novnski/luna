# OpenCode Go native probe results and replay commands

Updated 2026-10-08 after reviewing all completed Go probes. **All 19 probe
processes exited 0.** The fixed adapter source is committed as
`8517ae3353256d21cdb2a5eed35475b83fbb27d2` (`fix(opencode): reconcile interrupted
streams and emit failures once`); the coordinator reports it pushed. Runs used
OpenCode **2.0.24** and the existing isolated Go-profile wrapper, with binaries
built in the main checkout. The native compatibility report still says
**`Untested`**; this targeted evidence does not certify the full product matrix.

Actual artifacts are under `target/qa/claude-review-20261008/live-go/<probe>/`.
The commands below are repeatable examples; their timestamped `GO_OUT` keeps
future reruns separate. The existing wrapper is solely the provider-profile
entry point. Do not build in its old checkout, copy credentials, change its
configuration, or point the active GUI app at it.

## Completed evidence

Every listed probe has `exit-code.txt` containing `0` and a retained `probe.log`.
Structured summaries and native receipts were checked in addition to exits.

| Artifact subdirectory | Observed result | Proof boundary |
| --- | --- | --- |
| `plain` | Own prompt accepted, enqueued and delivered; nonempty completed answer; stream closed. | Live adapter admission and settlement. |
| `s1-allow`, `s1-deny`, `s1-cancel`, `s1-question` | One approval each for allow/deny/cancel; `GO-ALLOW`, `GO-DENIED`, and interrupted cancellation respectively. One question answered after 7 seconds returned `ALPHA`. | Scripted shared input bridge; no native graphical panel, multi-label selection or stale-answer UI proof. |
| `s2-subagents` | **Exactly two native child sessions/calls:** foreground `background=false`, background `background=true`. Both tagged child transcripts completed; root sent two completed segments with one provider wake; stream closed. | Requested foreground and background modes both occurred, although generic probe acceptance alone only requires one child. `native tasks: []`: this run did not prove background-shell task rows. |
| `s2-late-ask` | 9/9 structured checks: root parked, one child-attributed approval arrived after root Done, status awaited approval, answer resolved, child ran, separate completed wake segment, no cards left. | Production engine and provider, not graphical UI. |
| `s2-child-stop` | 10/10 checks: foreground child stoppable; stop receipt idempotent; child became interrupted; its shell stopped; root continued; no blocking work remained. | Stop acknowledgement and native terminal evidence both observed. |
| `s3-luna-tools` | 23/23 checks: first-turn delivery, resumed canonical names, contract withdrawal, redelivery and integration inventory. | Inventory includes an intentionally failed dummy MCP process; its displayed failure is expected, not evidence every integration connected. |
| `s3-child-containment` | Fresh/resumed root LUNA calls passed; child attempted `luna_get_workspace`, received `tool.execution / Unable to execute luna_get_workspace`, and produced no host LUNA invocation; probe passed. | The host boundary held; the generic native error does not identify which upstream rule blocked it. Not cross-project, forked or post-compaction engine proof. |
| `steer`, `stop-steer` | Steer delivered under the exact LUNA message ID and final answer included `KIWI`; stopped steer was withdrawn and never emitted a `Steered` boundary. | Live native steering and cancellation. |
| `s4-drop`, `s4-drop-late` | Both reconnected after the injected gap, detected missed durable frames and completed with one tool result and one final reply. Readbacks produced 5 and 11 synthetic frames respectively; pending-request readbacks were successful and empty. | Real native SSE loss; distinct from an app restart or a parked-child disconnect. |
| `s4x-server-kill` | All six summary checks passed: killed with a form pending, interrupted, one crash error and no duplicates, same-session continuation completed without a stray old chip. | Kills only the probe-owned provider server; not the GUI process. |
| `s5-history` | All checks passed: three checkpoints, before/through/tip forks, native readback, invalid-boundary refusal, rewind, stale rewind refusal, continuation. Both recall observations contained ALPHA and excluded BETA/GAMMA; `beta.txt` was preserved. | Native history contract; engine side chats and edit-earlier UI flow remain separate. |
| `s6-s7-inputs-models` | First PNG was correctly identified without tool reads after staged-file removal; boundary JPEG reply named red. MiMo→Haiku `low` runtime readback matched. **Two manual compactions completed natively:** idle in 2.903 s and while a turn ran in 3.151 s; both emitted `session.compaction.started` and `.ended`, with Haiku `low` and `default` respectively. Text-only and SVG rejection passed. | This proves supported idle/live compaction on these sessions. It does not prove lost-compaction-terminal recovery, automatic compaction, or mid-turn image steering; the image follow-up uses `AllowFollowUpTurn`. |
| `s7-discovery` | Project command/skill discovery and title generation passed; the added command appeared on the next turn and next `commands_in`. | **`REFRESHED_IN_RUN false`**: same-turn live catalog refresh was not observed. |
| `tools-output` | Structured summary: 85,816 live shell bytes in two chunks, native result marked truncated, full capture `complete=true`, 139,220 journal-output bytes, 86,030 display bytes, both markers found, document tool part resolved without error. | Full output capture/retrieval/render-helper proof; not a screenshot of the native detail view. |

### Observed invocation counts

Counts below were computed from the retained wire JSONL, deduplicating SSE
events by native event ID and prompt admissions by path/caller ID. They are
**not billed-request or billed-token counts**. Prompt posts include steers
(including the withdrawn steer); executions include children and wakes. The
two drop probes intentionally miss native events, so observed step counts are
a lower bound. Compaction and title-generation model calls are separate.

| Probe | Successful `/prompt` admissions | Observed execution starts | Observed model-step starts |
| --- | ---: | ---: | ---: |
| plain | 1 | 1 | 1 |
| s1-allow | 1 | 1 | 2 |
| s1-deny | 1 | 1 | 2 |
| s1-cancel | 1 | 1 | 1 |
| s1-question | 1 | 1 | 2 |
| s2-subagents | 1 | 4 | 7 |
| s2-late-ask | 1 | 3 | 5 |
| s2-child-stop | 1 | 2 | 3 |
| s3-luna-tools | 4 | 4 | 7 |
| s3-child-containment | 3 | 4 | 8 |
| steer | 2 | 1 | 2 |
| stop-steer | 2 | 1 | 1 |
| s4-drop | 1 | 1 | 2 |
| s4-drop-late | 1 | 1 | 1 |
| s4x-server-kill | 2 | 2 | 2 |
| s5-history | 5 | 5 | 6 |
| s6-s7-inputs-models | 5 | 6 | 5 |
| s7-discovery | 2 | 2 | 3 |
| tools-output | 1 | 1 | 2 |
| **Total** | **36** | **42** | **62** |

The 62 observed steps identify **59 Haiku 5.5** and **3 MiMo 2.6 Flash** steps,
all under `opencode-go`. In addition, the wire records **two successful manual
compaction admissions/completions** and **three successful `/generate` calls**
(child-stop, LUNA-tools and discovery probes). Those generate request bodies
contain only a prompt, so their chosen model is not assigned from these bodies.

The metadata catalog alone has a reviewed secret-scan result and SHA256SUMS.
At this review, no final whole-`live-go` scan/manifest was present; the coordinator
should run the final scan/hash commands below after artifact collection ends.

## Live metadata and model choice

The wrapper reports **OpenCode 2.0.24**. A short-lived private native server
returned 41 models, including 30 enabled `opencode-go` entries. It received only
`GET /api/info` and `GET /api/model`, then its stdin lease was closed and its
process exited. Safe catalog evidence:
`target/qa/claude-review-20261008/live-go/catalog/native-models.json`.

Use **`opencode-go/claude-haiku-5-5`** as the small general QA model: the live
catalog advertises tools, image/PDF input, and `none/low/medium/high/xhigh/max`
variants. Catalog base input/output prices are 0.1/0.5. This is a practical
small-model choice, not a measured latency or quality result.

Use **`opencode-go/mimo-v2.6-flash`** for the first image turn in the combined
probe, then switch to Haiku with `low`; MiMo advertises tools and image input,
base input/output prices 0.14/0.28, and no variants. Both are paid-provider IDs;
an enabled catalog row alone does not prove quota or generation. The completed
paid probes above now provide actual successful generations for both models.

The one-shot CLI `models` returned an empty list, as did an immediate standalone
API read. The retained catalog came from a readiness-checked persistent private
server. Main's existing `opencode_models_probe` binary was dated October 6 and
did not finish discovery; only that metadata process was terminated. The
coordinator subsequently rebuilt the selected examples in main for this batch.

## Main-checkout build, once

The coordinator owns build-budget admission and ran the required builds. These
commands reproduce them; this reviewer performed artifact review only.

```sh
cd /Users/iulian/workspace/code/personal/luna
scripts/cargo.sh build --locked -p luna-harness \
  --example opencode_models_probe \
  --example opencode_input_probe \
  --example opencode_subagent_probe \
  --example opencode_steer_probe \
  --example opencode_history_probe \
  --example opencode_inputs_models_probe \
  --example opencode_luna_mcp_probe \
  --example opencode_discovery_probe \
  --example opencode_server_kill_probe

scripts/cargo.sh build --locked -p luna-engine \
  --example opencode_tool_output_probe \
  --example opencode_luna_tools_probe \
  --example opencode_late_ask_probe \
  --example opencode_child_stop_probe
```

## Command setup and evidence capture

Use the function in the same shell as the probe commands. Run one probe at a
time and inspect any nonzero exit before continuing. Every invocation retains
its exact exit status and redacted native wire log under the main checkout.
The probes create disposable provider sessions in the existing isolated Go
profile; their own working fixtures and engine stores are disposable.

```sh
cd /Users/iulian/workspace/code/personal/luna
export OPENCODE_EXECUTABLE='/Users/iulian/workspace/code/personal/luna-worktrees/qa-opencode/target/luna-qa-data/opencode/opencode-profile/bin/opencode'
GO_MODEL='opencode-go/claude-haiku-5-5'
GO_IMAGE_MODEL='opencode-go/mimo-v2.6-flash'
GO_BIN="$PWD/target/debug/examples"
GO_OUT="$PWD/target/qa/claude-review-20261008/live-go/run-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$GO_OUT"
agent-python scripts/qa-opencode-manifest.py binary --path "$OPENCODE_EXECUTABLE" > "$GO_OUT/binary-resolution.json"
git rev-parse HEAD > "$GO_OUT/source-head.txt"
git diff --binary -- crates/harness/src/opencode > "$GO_OUT/adapter.patch"

run_go() {
  local probe_label="$1"
  shift
  mkdir -p "$GO_OUT/$probe_label/wire"
  LUNA_OPENCODE_WIRE_LOG="$GO_OUT/$probe_label/wire" "$@" > "$GO_OUT/$probe_label/probe.log" 2>&1
  local probe_rc=$?
  printf '%s\n' "$probe_rc" > "$GO_OUT/$probe_label/exit-code.txt"
  printf '%s exit=%s log=%s\n' "$probe_label" "$probe_rc" "$GO_OUT/$probe_label/probe.log"
  return "$probe_rc"
}
```

## Small initial proof and S1 inputs

```sh
run_go plain "$GO_BIN/opencode_steer_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" plain

run_go s1-allow env PROBE_MODE=ask PROBE_APPROVAL='Allow once' \
  PROBE_MIN_APPROVALS=1 PROBE_EXPECT_TEXT=GO-ALLOW \
  "$GO_BIN/opencode_input_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" \
  'Call your direct shell tool exactly once to run: printf GO-ALLOW. Reply with only its output.'

run_go s1-deny env PROBE_MODE=ask PROBE_APPROVAL=Deny \
  PROBE_MIN_APPROVALS=1 PROBE_EXPECT_TEXT=GO-DENIED \
  "$GO_BIN/opencode_input_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" \
  'Call your direct shell tool to run: printf SHOULD-NOT-RUN. If denied, do not retry; reply exactly GO-DENIED.'

run_go s1-cancel env PROBE_MODE=ask PROBE_APPROVAL='Cancel turn' \
  PROBE_MIN_APPROVALS=1 PROBE_EXPECT=interrupted \
  "$GO_BIN/opencode_input_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" \
  'Call your direct shell tool to run: printf SHOULD-NOT-RUN. Wait for its result.'

run_go s1-question env PROBE_MODE=full PROBE_CHOICE=0 \
  PROBE_MIN_QUESTIONS=1 PROBE_EXPECT_TEXT=ALPHA PROBE_ANSWER_DELAY_MS=7000 \
  "$GO_BIN/opencode_input_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" \
  'Use your question tool to ask one single-choice question with options ALPHA and BETA. Wait for the answer, then reply with only the selected label.'
```

The delayed question also crosses the native keep-alive idle boundary. This
scripted input bridge is not native UI proof. The existing probe selects one
label for ordinary question panels; it does not prove multiple simultaneous
selections or the stale-answer UI fence.

## S2 native children, late asks and per-child Stop

```sh
run_go s2-subagents "$GO_BIN/opencode_subagent_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" \
  'Use your subagent tool to launch one foreground general child named Foreground that runs printf FG-CHILD and reports its output. Then launch one background general child named Background that runs sleep 2 followed by printf BG-CHILD and reports its output. Reply ROOT-DONE without waiting for the background child.'

run_go s2-late-ask "$GO_BIN/opencode_late_ask_probe" \
  "$OPENCODE_EXECUTABLE" "$GO_MODEL" "$GO_OUT/s2-late-ask/artifacts"

run_go s2-child-stop "$GO_BIN/opencode_child_stop_probe" \
  "$OPENCODE_EXECUTABLE" "$GO_MODEL" "$GO_OUT/s2-child-stop/artifacts"
```

The child/late-ask engine probes retain structured artifacts and test the
production engine against the real provider. The generic subagent probe's exit
condition requires at least one settled tagged child; inspect its child summary
to confirm the requested foreground **and** background children both appeared.
Background shell native-task rows and graphical child panels remain separate.

## S3 first-turn/resumed LUNA tool delivery

```sh
run_go s3-luna-tools "$GO_BIN/opencode_luna_tools_probe" \
  "$GO_MODEL" "$GO_OUT/s3-luna-tools/artifacts"

run_go s3-child-containment "$GO_BIN/opencode_luna_mcp_probe" \
  "$GO_MODEL" fresh resume subagent
```

The engine probe covers first turn, resume, withdrawn-contract fallback,
redelivery, and integration inventory. The harness probe adds native-child
tool containment. Neither is a substitute for S3's cross-project, forked and
post-compaction GUI/RPC scenarios.

## S4 transport loss, steering, native-server death

```sh
run_go steer "$GO_BIN/opencode_steer_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" steer
run_go stop-steer "$GO_BIN/opencode_steer_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" stop-steer
run_go s4-drop "$GO_BIN/opencode_steer_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" drop
run_go s4-drop-late "$GO_BIN/opencode_steer_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" drop-late

run_go s4x-server-kill env PROBE_KIND=form PROBE_OUT="$GO_OUT/s4x-server-kill/summary.json" \
  "$GO_BIN/opencode_server_kill_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL"
```

The last probe intentionally kills only the native server it owns while a form
is pending, then resumes that provider session. It does not restart or kill the
active GUI app. Full S4 app restart, durable queued rows, and stale input fences
still require the separate app scenario.

## S5 native history; S6/S7 images, model switching and compaction

```sh
run_go s5-history "$GO_BIN/opencode_history_probe" \
  "$OPENCODE_EXECUTABLE" "$GO_MODEL" scenario "$GO_OUT/s5-history/workspace"

run_go s6-s7-inputs-models "$GO_BIN/opencode_inputs_models_probe" \
  "$OPENCODE_EXECUTABLE" "$GO_IMAGE_MODEL" "$GO_MODEL" low

run_go s7-discovery "$GO_BIN/opencode_discovery_probe" "$GO_MODEL"
```

History covers three marker turns, native forks before/through/tip, fork recall,
rewind, stale boundary rejection, and preservation of workspace files. It does
not cover engine side chats, UI history approvals or edit-earlier-message RPCs.
The combined probe covers native image-only input, a boundary follow-up image,
model/variant readback, idle and live manual compaction, and local rejection.
Its “steer image” is `AllowFollowUpTurn`, not the UI's mid-turn image-steer path.
Discovery covers project commands/skills, command refresh and a generated title.
S6 usage-store/limits proof remains separate.

## Full tool output through journal, document and retrieval

```sh
run_go tools-output env PROBE_MARKERS=GO-HEAD,GO-TAIL \
  "$GO_BIN/opencode_tool_output_probe" "$OPENCODE_EXECUTABLE" "$GO_MODEL" \
  "$GO_OUT/tools-output/artifacts" \
  'Call your direct shell tool exactly once to run: printf "GO-HEAD\n"; i=0; while [ "$i" -lt 2200 ]; do printf "payload-row-abcdefghijklmnopqrstuvwxyz\n"; i=$((i+1)); done; printf "GO-TAIL\n". Then reply DONE. Do not use any other tool.'
```

This checks head/tail markers after native truncation thresholds, live shell
activity, host journal retrieval, folded document parts and display rendering.
Inspect `summary.json` plus `tool-*-output.txt` and `tool-*-display.txt`.

## Final evidence and proof boundary

```sh
agent-python scripts/qa-opencode-manifest.py scan --dir "$GO_OUT"
agent-python scripts/qa-opencode-manifest.py sums --dir "$GO_OUT"
```

Record actual provider/model/session and turn counts from receipts/wire events;
native children, retries and compaction can generate more than one provider
request per probe. A zero exit plus retained native evidence is live harness or
engine proof for the stated surfaces. It is not T4 graphical proof or a full
S1–S8 pass. **S8 review/goals has no equivalent in these existing probes**;
retain that as a separate engine/native-UI scenario. The active GUI app keeps
its separate new isolated profile for T2/manual UI, and its results must not be
attributed to this paid Go profile.
