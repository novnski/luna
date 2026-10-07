# OpenCode v2 parity: validation plan

Research date 2026-10-06/07. The failure cases in §3 are written **before** any implementation, as the user's rule requires; each needs an expected outcome recorded before its package's code is written. The E2E scenarios in §4 are the acceptance proof for the [implementation plan](implementation-plan.md). **Nothing here has been run.** All scenarios (OCE-00..24) are planned, and every matrix row's Verification is `unverified` until a retained artifact exists. Draft sources: [notes/D-reconciliation-verification.md](notes/D-reconciliation-verification.md) §5, extended by the coordinator with v2.0.22-specific cases (§3.P) and scenarios OCE-00 and OCE-19..23.

## 1. Execution rules for future runs

These are binding, from `AGENTS.md` and `docs/reference/qa-infrastructure.md`.

**App and identity**
1. Run **one LUNA GUI app at a time**, QA and installed copies included. `scripts/run-macos-dev.sh` refuses a second app and serializes app operations across worktrees; exit 75 means the slot is busy, so coordinate. Before native QA, let the current app save and quit gracefully. Never force-quit. If it declines to quit, leave it and report.
2. A QA bundle requires all four overrides: `LUNA_DEV_APP_NAME`, `LUNA_DEV_BUNDLE_ID`, `LUNA_DEV_IPC_PORT`, `LUNA_DEV_DATA_DIR`. Use a **dedicated OpenCode QA identity** assigned in OC-01. Verify `codesign --verify --deep --strict`, `plutil -p Info.plist`, `lsof -iTCP:<port> -sTCP:LISTEN` and `ps -p <pid>`. Never refresh main's identity from a QA worktree.

**Builds**
3. Use `scripts/cargo.sh` with `--locked`. Run `python3 scripts/build-budget.py --status` before builds. Use `CARGO_INCREMENTAL=0` for one-off validation. Respect the 170 GiB aggregate ceiling and per-build reservation; never bypass the guard.

**Isolation and evidence**
4. **OpenCode storage isolation** for every QA run: temporary `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` (or `OPENCODE_CONFIG_DIR` + `OPENCODE_DB`) and `OPENCODE_DISABLE_AUTOUPDATE=1`, plus an ordinary disposable Git fixture repo (no worktree-creating harnesses). Record in the manifest whether storage was isolated. Never read the user's credential store for QA.
5. Keep evidence outside disposable checkouts (`target/qa/opencode/<scenario>/run-*` with a `latest` pointer, or an external evidence dir). After review: hash the evidence, confirm nothing needs the bundle, quit the QA app gracefully, unregister and remove only that bundle, keep its data and the cargo cache, and reopen the primary **LUNA Dev**.

**Model calls**
6. **PD-18 (user, 2026-10-07): real-AI testing is authorized for every scenario,** including the Claude comparison runs. Every OCE scenario needs a real-model (T3) pass for full confidence. Fake-AI (T2) runs stay in use for deterministic fault injection (forced provider errors, dropped connections) and fast repeatable regressions. Accounts: OpenCode runs default to OpenCode's free models inside the isolated QA profile (no login needed). If a paid OpenCode model is needed, the user signs in to that QA profile themselves; the agent never copies keys. Claude comparison runs use the user's normal Claude Code login. Report model usage after each batch.

**Labelling**
7. Never label T0 or T1 results as real-provider evidence. A screenshot proves presentation only. A successful RPC write proves admission, not consumption. A model's claim is not verification.

**Parity proof (PD-14)**
8. For shared-surface scenarios (OCE-01, 06, 08, 15, 16, 17, 20, 24), run the same scripted steps against **Claude Code** (or Codex where Claude lacks the surface) and OpenCode in the same fixture repo. Diff the normalized LUNA artifacts: `AgentEvent` kinds, doc part kinds and layout, receipts, approval and question payload shapes. Pass means OpenCode differs only where its native behaviour genuinely differs, and each such difference is recorded in `result.md`. Comparison runs use real Claude and real OpenCode models (authorized, PD-18); a mock or recorded transport may additionally be used for fast structural diffs.

## 2. Evidence artifact contract

This extends the Claude parity contract (`docs/pending-tasks/claude-code-feature-parity/validation-plan.md:65-77`). Each scenario run directory contains the following.

1. **`manifest.json`**
   - Scenario id, UTC start and end.
   - LUNA SHA and dirty flag.
   - App bundle name, id, port, pid and executable, plus the codesign result.
   - **OpenCode binary resolution record:** resolved path, wrapper chain (e.g. `opencode2` → `opencode`), SHA-256, `--version`, resolver source (env, process PATH, login-shell `-l -i -c`/`-l -c`, install path, node-manager), the engine's PATH and the launch context (terminal, launchd GUI, headless, remote).
   - **Generation and compatibility record:** `/api/info` status and body; legacy probe results; OpenAPI subset fingerprint; known-good/known-bad/untested class; the event-vocabulary version.
   - Server ownership: spawned via `--stdio` or external. URL and port, never credentials.
   - XDG directories in effect and whether isolated.
   - Requested vs **effective** provider, model, variant and agent, from server readback.
   - LUNA permission mode and the compiled OpenCode rules.
   - LUNA MCP attachment and instruction-entry readback.
   - Evidence tier.
2. **`commands.jsonl`:** every command and RPC, with env var **names** only, exit, duration and log paths.
3. **`native-frames.jsonl`:** ordered raw `/api/event` frames with receipt time, connection generation and `durable.seq`. Plus **`http.jsonl`:** method, path, status, request and response body hashes, and redacted bodies for prompt, command, permission and form replies, interrupt, fork, revert and compact. Include a redaction manifest.
4. **`native-readback/`:** session record, cursor-paged message list, child sessions, inbox, pending permissions and forms, `session/active`, captured before and after each disruptive step.
5. **LUNA state:** `normalized-events.jsonl` (host journal), `state-before.json` and `state-after.json` (`WatchDocMessages` resets, `WatchQueue`, task receipts, `GetTaskIdentity`, bindings), and attachment manifests with the original SHA-256 and the hash of bytes sent in `files[]`.
6. **`observer.json`:** subscription lifecycle and snapshot timing. Persistent subscriptions only, no `WatchDocMessages` polling.
7. Screenshots and AX readback where UI behaviour is the claim. `SHA256SUMS`. A `result.md` with expected vs actual per criterion, unverified claims and replay steps. `pass.json` only when all criteria pass.

**Evidence tiers.** Every claim names its tier, and a lower tier never substitutes for a higher one.

| Tier | What runs | What it proves |
| --- | --- | --- |
| T0 | Fake HTTP/SSE through the real adapter (existing `tests.rs` style) | Transformation only |
| T1 | Recorded real `native-frames.jsonl` replayed through adapter → engine → doc → RPC | Ordering and recovery |
| T2 | **Real `opencode` v2 binary** plus a mock OpenAI-compatible provider in isolated XDG and workspace config (generalized `opencode_subagent_probe.rs`) | Real server lifecycle, events, permissions, forms, children; no model cost |
| T3 | Real binary plus a real model (authorized for every scenario, PD-18) | Model-visible consumption, e.g. images; required pass for every scenario |
| T4 | Signed QA app native UI on top of T2 or T3 | Native UI behaviour |

## 3. Failure cases (write the expected outcome before implementation)

IDs `OCF-nn` are stable. Each group lists the work packages that must address it. Historical precedents are cited where they exist.

**A. Binary, generation, environment (OC-00, OC-17)**
- OCF-01: launchd LUNA resolves v1 while a terminal engine resolves v2, so behaviour and storage differ by launch context.
- OCF-02: the `-l -i -c` probe fails, so the `-l -c` fallback finds v1 first. **Expected after PD-15:** v1 is skipped and the resolver keeps searching until it finds `~/.opencode/bin` v2.
- OCF-03: the override or PATH points at a wrapper (`opencode2`, `opencode1` with isolated XDG), so the manifest records the wrapper, not the real binary, hash or storage.
- OCF-04: discovery uses a different binary, generation, cwd or config than the run.
- OCF-05: v1 and v2 share XDG data. The v1 lazy migration fails on a v2-rewritten DB (`no such column: project_id`, `67c960f4`), or sessions vanish across generations.
- OCF-06: the generation probe misclassifies (HTML on `/global/health`; `/api/health` absent).
- OCF-07: the binary is upgraded while a server runs, and resume hits schema or event drift.
- OCF-08: a headless daemon or remote host has a different PATH or HOME, so it uses a different binary and reports the wrong capabilities.
- OCF-09: the update checker reports the `~/.opencode/bin` binary while runs use Homebrew v1.

**B. Server lifecycle and transport (OC-02, OC-08)**
- OCF-10: a request during the boot window parks forever (precedent `0b421cc2`).
- OCF-11: the first prompt goes out before the SSE subscription is live and misses events (`a401432f`).
- OCF-12: SSE drops mid-turn and the gap's events are lost, so the turn hangs or completes falsely.
- OCF-13: a server crash or restart means the lost execution never terminates, pending asks vanish and MCP registrations are forgotten.
- OCF-14: an orphaned `opencode serve` after a LUNA crash; port collision; two servers on one storage.
- OCF-15: the OpenCode TUI or another client writes the same session concurrently.
- OCF-16: LUNA quits mid-turn and the orphan keeps editing files.

**C. Attribution, duplicates, ordering (OC-06, OC-07, OC-11)**
- OCF-20: the root goes idle while children run, so the root is marked Done and later child output is misattributed or dropped.
- OCF-21: child events are folded into the parent transcript.
- OCF-22: child part or tool ids collide with parent ids, overwriting the parent's output.
- OCF-23: a provider-initiated execution (background wake, revert-clear wake) settles a queued human receipt.
- OCF-24: duplicate frames duplicate text or tools.
- OCF-25: a delta arrives after the final part state, truncating or doubling text.
- OCF-26: a foreign session's event appears in a LUNA chat.
- OCF-27: `command` and `compact` turns have no client id and can't be correlated.
- OCF-28: a queue and a steer in flight both claim one terminal.

**D. Delivery, steering, queue (OC-07, OC-09)**
- OCF-30: a prompt POST times out but was accepted, so a resend duplicates it.
- OCF-31: steer acceptance is labelled as incorporation, and a later cancellation isn't reflected.
- OCF-32: a steer drops its attachments.
- OCF-33: a queue head with attachments is steered as text only.
- OCF-34: a Send-now interrupt races completion, so the new prompt lands in the aborted execution.
- OCF-35: the client id is reused on retry, so the server duplicates or rejects the prompt.

**E. Cancellation (OC-07, OC-11)**
- OCF-40: Stop aborts the root but children continue unseen.
- OCF-41: a pending permission stays answerable after Stop, and a late answer resurrects work.
- OCF-42: the abort response is lost, so LUNA shows Stopped while the server continues.
- OCF-43: an interrupted execution is reported as Completed.
- OCF-44: a Decline sent as reject-without-message ends the whole execution when the user meant "skip this tool".

**F. Permissions, questions, stale approvals (OC-04, OC-05)**
- OCF-50: an answer after turn end or restart applies to a new execution or another session.
- OCF-51: a child ask after the root goes idle is auto-declined or shown on the wrong chat.
- OCF-52: "always" persists a project-wide rule into the user's OpenCode state.
- OCF-53: Full compiles to `*: allow`, but special asks such as external-directory still park.
- OCF-54: concurrent asks: answering one resolves or hides the other.
- OCF-55: legacy auto-approve (2.0.3 era) approves for sessions LUNA doesn't own.
- OCF-56: a form with unsupported fields waits forever (BB `1f73cd73`).
- OCF-57: a question is answered twice, or a cancel races the answer.
- OCF-58: reconnect readback re-emits an ask, creating duplicate cards.

**G. Output fidelity and partial output (OC-06)**
- OCF-60: output above the preview cap is truncated before the journal.
- OCF-61: streaming shell output is lost or duplicated against the final output.
- OCF-62: tool error vs empty output, and unavailable vs empty, are conflated.
- OCF-63: tool attachments (read-tool images, file content) are dropped.
- OCF-64: reasoning is lost or rendered as answer text.
- OCF-65: partial text at an abort or crash is discarded or shown as complete.
- OCF-66: an unknown part or event type is dropped silently.
- OCF-67: diff metadata is lost or per-file identity is unstable.
- OCF-68: todo or structured updates are lost (v1 path).

**H. Provider failure (OC-07, OC-10)**
- OCF-70: an auth, quota or rate-limit error leaves the chat "Working" forever (`7c6c1236`).
- OCF-71: retry status is hidden or shown as a terminal failure.
- OCF-72: an unavailable model or variant is silently replaced.
- OCF-73: context overflow is confused with auto-compaction.
- OCF-74: a 5xx on session create is retried blindly, creating duplicate sessions.
- OCF-75: a resume lookup failure silently starts a fresh session (regression of the 2026-09-26 fix).

**I. Images and attachments (OC-09)**
- OCF-80: an image is sent as path text on some route.
- OCF-81: an image-only prompt is rejected or sent empty.
- OCF-82: a model without image input silently ignores the image.
- OCF-83: a remote-device image path is resolved on the client, not the host.
- OCF-84: a restart between staging and dispatch loses the attachment or dispatches a stale `pending://`.
- OCF-85: retrying a failed turn drops its images.
- OCF-86: an oversized, corrupt or SVG file passes admission.
- OCF-87: editing a queued row duplicates or drops its image.

**J. History, restart, reopen (OC-08, OC-12)**
- OCF-90: the reopened doc diverges from the provider message list.
- OCF-91: on restart with a running turn, LUNA says interrupted while the server still runs, or the reverse.
- OCF-92: a fork mutates the source, or copies rules or MCP registration unexpectedly.
- OCF-93: a revert restores files, contrary to LUNA's "files kept" rule; or a staged revert is committed by the next prompt.
- OCF-94: an imported TUI session reopens with the native `plan` agent active (PD-1).
- OCF-95: a session deleted externally silently breaks the binding.

**K. Models, variants, discovery (OC-10, OC-14)**
- OCF-100: a catalog read before the models.dev sync is cached as empty.
- OCF-101: a variant is persisted for a model that doesn't offer it.
- OCF-102: a discovery server leaks, or a heavy decode regresses (112 MB precedent).
- OCF-103: a connected-only filter hides models usable via env keys.
- OCF-104: a mid-session model change isn't reflected in readback.
- OCF-105: the native agent list surfaces `plan` as a LUNA mode.

**L. Compaction (OC-10)**
- OCF-110: auto-compaction mid-turn is treated as turn end, or later output is dropped.
- OCF-111: manual compact is uncorrelated, and LUNA's tool bootstrap is lost afterwards.
- OCF-112: the context ring is stale after compaction.
- OCF-113: a compaction failure leaves the turn hanging.

**M. Native subagents vs LUNA child chats (OC-11, OC-13)**
- OCF-120: a native subagent is shown as a persistent LUNA chat, or the reverse.
- OCF-121: `create_thread` is advertised to OpenCode without verified `luna` MCP delivery.
- OCF-122: a cross-provider child of an OpenCode parent inherits OpenCode's model, variant or account.
- OCF-123: a LUNA-created OpenCode child gets wider permissions than the parent ceiling.
- OCF-124: a background subagent completes after the parent turn, and the wake execution is unowned.
- OCF-125: Stop misses native subagents, or wrongly stops independent LUNA child chats.

**N. Remote device (OC-16)**
- OCF-130: the remote client shows local OpenCode capabilities instead of the host's.
- OCF-131: a remote viewer answers an approval without authority.
- OCF-132: remote attachments resolve on the wrong device.
- OCF-133: a host restart leaves the remote view stuck in Running.

**O. QA and evidence integrity (OC-01, OC-18)**
- OCF-140: a QA run uses the user's real OpenCode config, auth, MCP or storage.
- OCF-141: a free-model run consumes quota or credentials unexpectedly.
- OCF-142: evidence captures auth headers or keys.
- OCF-143: T0 or T2 evidence is labelled as real-model proof.

**P. v2.0.22 contract specifics found in this research (coordinator)**
- OCF-150: `/api/health` is absent, so readiness never succeeds and the run fails after 300 s with no actionable message (G-0). **Expected after OC-02:** v2 detected via `/api/info` within the boot time, and a clear error if boot itself fails.
- OCF-151: the permission reply returns 400 and LUNA marks it resolved while the native tool waits forever (G-1).
- OCF-152: the slash command returns 400, is ignored, and the turn stalls 60 s (G-5).
- OCF-153: LUNA's `permission` key is ignored and OpenCode defaults govern the mode. Or, if the server is strict, every session create fails 400 (Q-3).
- OCF-154: an agent shell command reads the server password from its environment and self-approves via the API (G-4).
- OCF-155: the persisted fork checkpoint is the synthetic `"usage"`, so a later fork fails "checkpoint usage is not in source session" (G-7).
- OCF-156: compaction posts `null` (400), or is recorded "completed" on admission, then the server is killed before compaction runs (G-13).
- OCF-157: LUNA SIGTERMs mid-turn, and the user's managed background service later resumes LUNA's turn unobserved, possibly while LUNA's server is still alive (G-8).
- OCF-158: an MCP elicitation form owned by the `"global"` sentinel shows in the wrong chat, or nowhere.
- OCF-159: cumulative session tokens are shown as context occupancy (G-14).
- OCF-160: a v2 `subagent` call isn't bound and the child transcript disappears (G-6).
- OCF-161: a v2 `shell` call renders as a generic card with the command stripped, and the approval is untyped (G-6).
- OCF-162: a new upstream event type is dropped silently, with no diagnostic.
- OCF-163: an OpenAPI field rename between releases is detected only by timeouts or 400s instead of the boot fingerprint.
- OCF-164: an untested version runs without any visible degraded-mode notice, or is refused although compatible.
- OCF-165: a LUNA-spawned server exposes the embedded web UI on loopback with a guessable lifecycle.
- OCF-166: a websearch provider-selection form blocks a turn invisibly.
- OCF-167: a background subagent completes via `session.synthetic`, a parent execution starts that LUNA never requested, and its output is folded into the previous turn or dropped.
- OCF-168: a native `session.moved` changes the working directory outside the chat's project, and LUNA keeps operating on the old directory or silently accepts the new one.

## 4. E2E scenarios

Common preconditions:
- Pins refreshed (OC-00); installed version recorded.
- A single signed QA app with all four overrides and the OpenCode QA identity.
- OpenCode storage isolated.
- An ordinary disposable Git fixture repo.
- An evidence directory per §2.
- Persistent subscriptions opened before the first send.
- The tier named per step.

| ID | Scenario (tier) | Steps | Expected evidence | Pass criteria | Failure refs | WPs |
| --- | --- | --- | --- | --- | --- | --- |
| **OCE-00** | **Baseline reproduction of the current blocker** (T2; run with the unmodified LUNA build before any change) | Start a QA app on the current code with v2.0.22 resolved; send one prompt; separately, attach the T2 rig with v1.18.34 | Manifest with resolution and generation probes (`/api/health` 404, `/global/health` HTML, `/api/info` 200); stderr tail; LUNA error text; wall-clock to failure | Records G-0 exactly as predicted (about 300 s "did not become healthy"), or **refutes** it; either outcome is kept. The v1 run completes. | OCF-06, 150 | OC-00, OC-01 |
| OCE-01 | Root/child attribution, with Claude comparison (T2 → T3) | Run the same script against Claude and OpenCode. Root spawns two native subagents, one foreground and one background, each running a marked shell command (`echo CHILD-A`, `echo CHILD-B`); root prints `ROOT-DONE` before the background child finishes; set `experimental.subagent_depth: 2` and have child A spawn a grandchild | Raw frames with `sessionID`/`parentID`; child-session readback; normalized events with spawn ids; parent, child and grandchild docs; journal per tool | Every tool output appears exactly once under the right spawn; no child text in the root; background completion attributed to its spawn; the wake execution is a provider turn; part ids unique across sessions; grandchild visible | OCF-20–23, 26, 124, 160, 167 | OC-07, OC-11 |
| OCE-02 | Duplicate and out-of-order events (T1 replay of a T2 capture) | Replay with (a) every frame duplicated, (b) deltas after the final state, (c) terminal before the last part, (d) an ask re-emitted, (e) an unknown event type injected | Replay inputs and diffs; normalized events; doc; journal hashes; diagnostics log | Output byte-identical to the clean run; one chip per call; one approval card; terminal unchanged; one bounded diagnostic for the unknown type | OCF-24, 25, 28, 58, 66, 162 | OC-06, OC-08 |
| OCE-03 | Cancellation (T2 → T3) | (a) Stop during a 20 s marked command; (b) Stop while a child runs, plus per-child stop; (c) Stop with a pending permission; (d) Send now during a running turn; (e) drop the interrupt response | Interrupt requests and responses, `execution.interrupted{reason}`, child readback, process list for the spawned shell, LUNA status and queue | Root and children stop, or remaining work is shown; the pending ask is closed; the Send-now prompt starts only after the interrupt; a lost interrupt shows "stop unknown" until readback; interrupted never shows as Completed | OCF-34, 40–44, 125 | OC-07, OC-11 |
| OCE-04 | Reconnects (T2 with an SSE fault-injecting proxy; T3 corroboration) | Break SSE (a) mid-text, (b) mid-tool, (c) with an ask pending, (d) with a background child running; (e) restart the server mid-turn; (f) overflow the 4,096-frame queue with a slow reader | Generation-tagged frames, `durable.seq` gaps, session-log replay, readback, LUNA doc | No duplicated or lost content compared with readback; pending ask re-shown once and answerable; running session continues; lost execution interrupted with a reason; bounded retry, then a truthful failure | OCF-12, 13, 58 | OC-08 |
| OCE-05 | Stale approvals and policy (T2) | (a) answer after Stop; (b) answer after app restart; (c) child ask after root idle; (d) two concurrent asks answered in reverse order; (e) Full mode hitting an external-directory ask; (f) "allow for session"; (g) **self-approval probe**: agent runs `env` and tries `curl` against its own server | Permission ids and replies; `session.permissions` readback; OpenCode storage diff before and after; probe output | Stale answers have no effect; concurrent asks independent; child asks routed to the right chat and capped at the parent ceiling; no project-wide rule written; Full never parks; **the password isn't in the agent's environment** | OCF-41, 50–55, 57, 151, 153, 154 | OC-02, OC-03, OC-04 |
| OCE-06 | Partial output and tool fidelity (T2/T3) | Shell command printing a head, >64 KiB of body and a tail, with mid-run output; edit and patch with diffs; read of an image; **webfetch of a fixture URL and a websearch through a mock provider, including a provider 429 → failover**; Stop one run mid-stream; crash the server in another | `tool.progress` frames, final tool event, journal, lazy-fetch result, doc preview size, diff rendering, screenshot | Full head and tail retrievable after reopen; live tail before completion with no duplicated final output; diffs rendered; image visible; partial output kept and labelled; empty ≠ unavailable; `shell` shown typed with its command; web cards show the URL or query, the result body via full-output fetch, and the failover or failure state | OCF-60–67, 161 | OC-06 |
| OCE-07 | Provider failure (T2 mock returning 401/429/500/overflow; T3 invalid model) | Per error class, send a turn; also an unreachable resume id, a 5xx on create, `finish: length` and `content-filter` | Error frames (`retry.scheduled`, `step.failed`, `execution.failed` with status and body), HTTP responses, LUNA status and visible errors | No forever-Working; retry visible and temporary; distinct from failure; finish reasons shown; no silent model substitution; resume failure sends nothing; create retried at most once | OCF-70–75 | OC-07 |
| OCE-08 | Images on every delivery path (T2 at the HTTP boundary; T3 for model-visible content) | (1) image-only initial; (2) follow-up text + image; (3) queued row with image delivered at idle; (4) steer with image while busy; (5) retry after a failed turn; (6) app restart between staging and dispatch; plus invalid, oversized and SVG files and a model without image input | Staged file SHA-256; captured `files[]` request bodies; message-part readback; model naming a fixture property (T3); receipts | The same hash reaches the provider on every path; no path-text substitution; invalid files rejected before acceptance with the draft recoverable; a capability notice where relevant | OCF-32, 33, 80–87 | OC-09 |
| OCE-09 | Restart recovery and claim safety (T2/T3, signed QA app) | Graceful app quit (a) idle, (b) running turn, (c) pending ask, (d) queued rows; relaunch. Then **start a managed OpenCode service on the same isolated storage** and check it doesn't resume LUNA's turn. Kill the LUNA app process (fault) and check for an orphan server | Process list, readback before and after, queue snapshots, receipts, doc, managed-service log | Recovered state equals provider truth; no orphan server (stdin lease); no surviving restart claim (interrupt before teardown); the queue starts paused; no automatic replay of unknown | OCF-14, 16, 84, 91, 157 | OC-02, OC-08 |
| OCE-10 | History reopen, fork, rewind, import (T2/T3) | Three marker turns with tools and a child; reopen; fork at turn 2; rewind before turn 2 with unrelated modified files present; import a TUI-created session, including one with the `plan` agent active | Message lists for source and destination; file hashes; doc; binding ids; checkpoint ids | Reopened doc matches provider history; fork leaves the source unchanged and uses a real `msg_` boundary (not `"usage"`); no orphan; rewind keeps files (`files:false`) and the continuation can't recall removed markers; the plan-agent session is safe with no Plan/Build UI | OCF-90, 92–95, 155 | OC-03, OC-12 |
| OCE-11 | Remote device (two engines via the `scripts/e2e-smoke.sh` pattern plus real OpenCode on host A; T2) | Device B views and queues into a chat hosted on A; sends an image from B; answers an approval from B as owner and as a read-only viewer; restart A | Both engines' docs, the host journal, the prompt request on A with host paths, authority checks, A's resolution record | Capabilities and models are A's; image bytes resolved on A; only authorized answers accepted; B converges after A restarts | OCF-08, 83, 130–133 | OC-16 |
| OCE-12 | Binary and generation resolution, compatibility and updates (E1-style checks plus one T2 boot per environment; no model) | Start the engine from (a) an interactive terminal, (b) the launchd GUI app, (c) a minimal-env daemon, (d) a remote host. For each, record the resolver decision, `/api/info` and the fingerprint. Repeat with the `-l -i -c` probe forced to fail, and with `OPENCODE_EXECUTABLE` set to v1, v2 and a wrapper. Tamper with a copy of the OpenAPI subset. Compare the update-check offer with the binary in use. Check that LUNA-spawned servers don't serve the embedded web UI | Resolution records per environment, compatibility class, storage dir, update-check output | One deterministic **v2** binary per environment, used for discovery, run and update. A v1 binary earlier on the PATH is skipped; an only-v1 install shows "OpenCode 2 required" with install instructions and never hangs (PD-15); shared-storage hazard noticed; a tampered fingerprint fails before the first prompt; untested versions degrade with a notice; the update offer matches the running binary | OCF-01–09, 150, 163–165 | OC-00, OC-02, OC-17 |
| OCE-13 | Models, variants, usage and accounts (T2/T3; listing needs no prompt) | List models right after boot and after sync; select model and variant (including a non-standard variant); switch the variant mid-chat; disconnect a provider; run one turn and record usage and cost; open the accounts card; **open the Usage page and compare OpenCode history (tokens and cost per day and model) with the same turns' usage events and OpenCode's own `opencode stats --models --cost` (read-only)**; repeat while OpenCode is busy writing | Catalog responses with timestamps, picker snapshot, effective model and variant from readback, cost rows, read-only account status | No cached-empty catalog; valid variants; effective selection read back; cost shown; context window from the model limit; `plan` not offered as a mode; account status read-only with no secrets; Usage-page OpenCode history matches the turns run (within price-table estimation for subscription models), includes reasoning and cache tokens, and the scan never blocks while OpenCode is writing | OCF-100–105, 159 | OC-10, OC-16 |
| OCE-14 | Compaction (T2 with a small-context mock; T3 corroboration) | Trigger auto-compaction mid-turn; run manual compact; afterwards, ask the agent to call `get_workspace` (only if OC-13 is done) | `compaction.*` frames, message list, context-ring readback, MCP call trace | The turn continues and completes once; compaction shown as lifecycle, not turn end; "completed" only after `compaction.ended`; ring updates; LUNA tools survive compaction, or unavailability is reported | OCF-110–113, 156 | OC-10, OC-13 |
| OCE-15 | Native subagents vs LUNA child chats, both directions, with Claude comparison (T2/T3) | (a) native subagent via OpenCode's own tool; (b) LUNA `create_thread` from an OpenCode parent (expected `unavailable` until OC-13 is verified); (c) a Codex or Claude parent creates an OpenCode child with an explicit model and variant; (d) Stop the parent | Child-session readback vs LUNA lineage records, approval content, child execution config, compiled ceiling rules | A native subagent never becomes a persistent LUNA chat; OpenCode-parent creation is truthfully unavailable, or works after a verified harmless creation; the OpenCode child gets only the brief, the correct model and variant, and a ceiling ≤ the parent's; Stop semantics distinct | OCF-120–125 | OC-11, OC-13 |
| OCE-16 | Approvals and questions fidelity (T2 with a mock MCP server for elicitation) | Shell, edit, external-directory and webfetch asks; question tool with text, single- and multi-select; MCP elicitation with every field kind plus URL mode; websearch provider form; cancel; timeout; Deny-with-feedback vs Cancel | Ask payloads vs the approval details shown; question panel AX readback; form replies (`answer`); 409 handling | Command, diff and resources visible; Deny continues and Cancel stops; forms use the shared panel; unsupported fields decline visibly; no hang anywhere; the approval card's AX roles match its behaviour (inherited shared defect RS-1: radio role on immediate-action choices) | OCF-44, 53, 56, 57, 151, 158, 166 | OC-03, OC-04, OC-05 |
| OCE-17 | Steering and queue truth (T2, then T3) | (a) queue two rows during a turn; (b) explicit Steer once advertised; (c) cancel a pending steer; (d) a lost prompt response; (e) duplicate identical text twice; (f) a held 20 s marked command plus a steer; (g) two parallel tools plus two steers (order kept); (h) a steer sent after `execution.succeeded` or with a stale id; (i) Stop with a pending steer, then the `interrupt?resume=true` variant; (j) a command and a compact in flight | Inbox frames, prompt request ids, `WatchQueue`, receipts, provider message list | Accepted, recorded and unknown are truthful; no duplicate user messages; no automatic replay of unknown; steering advertised only with native in-turn delivery; a steer counts as incorporated only on `session.inbox.delivered` and is cancelled on `inbox.cancelled` (row restored); in-flight tools finish before incorporation; two steers keep their order; a post-turn or stale steer is refused or becomes a queued turn per contract, never silently dropped; Stop with pending steers clears or resumes them exactly as requested | OCF-27, 28, 30–35, 40 | OC-07 |
| OCE-18 | LUNA tool delivery (T2) | Start fresh, resumed, forked and post-compaction OpenCode sessions; list attached MCP tools; read instruction entries; call `get_workspace`; attempt a cross-project request; restart the server and re-check; **a native subagent lists tools and attempts one `luna` call (expected: refused, as for Codex and Claude)** | MCP status readback, tool catalog hash, instruction-entry readback, call trace, authority rejection | Tools attached on all four start kinds (or delivery reported `Unsupported`); bootstrap ≤800 bytes in the instructions channel, not user text; cross-project denied; re-attached after restart; nothing written to the user's OpenCode config; a native child's `luna` call is refused by the evidence fence and the subagent deny rule | OCF-111, 121 | OC-13 |
| OCE-19 | Containment of OpenCode-only state changes (T2) | The agent attempts `opencode_session_move` to a sibling directory (inside and outside the project); the agent calls `opencode_session_rename`; another client (`opencode api`) deletes the session, switches its model, or **prompts the same session while LUNA's turn runs** | Permission frames (move denied), `session.renamed`/`deleted`/`model.selected` frames, LUNA doc and binding | The move is denied by LUNA's rules, so the workspace stays LUNA-owned as for Codex and Claude; native renames don't change the LUNA title (`run_title` owns it); an external model switch shows in the effective-config readback; a concurrent external prompt is attributed as a provider turn, not merged into LUNA's turn; a deleted session shows a recoverable broken binding | OCF-15, 95, 168 | OC-04, OC-12, OC-14 |
| OCE-20 | Discovery parity: commands, titles, skills (T2, compared with a Claude chat in the same fixture repo) | Project-only custom command; MCP prompt command; command with attachments; skill discovery and typed skill pick; `command.updated` and `skill.updated` refresh; first-turn title | `command.list` and `skill.list` responses, request bodies (`name`, `skills`), composer snapshot, `run_title` run | Project commands appear and run; failures visible; attachments forwarded; skills shown and sent as typed `skills[]` like Claude's; refresh without an app restart; title produced by `run_title`; composer behaves the same as for the Claude chat | OCF-04, 152 | OC-03, OC-09, OC-14 |
| OCE-21 | Background work through the shared native-task rows (T2) | Shell command with `background:true`; background subagent; Stop while both run; completion of each | `shell.*` frames, job status, `session.synthetic` completion, `NativeTaskChanged{Shell}` events, LUNA activity UI | Background shells show as the same native-task rows Claude's `local_bash` produces; background subagents show on their spawn card; completion attributed; the resulting provider turn is owned (`ProviderTurnStarted`); Stop applies the shared policy | OCF-124, 167 | OC-11 |
| OCE-22 | MCP, Code Mode and plugin tools (T2 with a mock MCP server and a local test plugin in the isolated config) | Direct MCP tool call; MCP tool via Code Mode `execute`; MCP resource list and read; plugin tool; `mcp.status.changed` during a run | Tool frames (are nested calls emitted?), cards, journal, inventory readback | MCP tools render as `Mcp{server, tool}` with input preserved; nested Code Mode calls visible if emitted (Q-12), else the Code Mode card says so; resources rendered; read-only status updates | OCF-63, 66 | OC-06, OC-14 |
| OCE-23 | Advanced-workflow parity (T2) | Trigger LUNA Review on an OpenCode chat for each target (uncommitted, commit, branch); ask for goals; check schedules | UI state, request bodies (`/review` command), receipts | Review is routed to OpenCode's `/review` command and labelled exactly as Claude's command-line review; goals and schedules are reported unavailable with the shared copy; no duplicate review UI | — | OC-15 |
| OCE-24 | Side chats from an OpenCode parent, compared with a Claude parent (T2) | From the same three-turn parent transcript in each provider: (a) native-fork side chat at turn 2; (b) snapshot side chat with a selected excerpt; (c) a read-only parent (expected refusal for both providers); (d) send two messages in each side chat; (e) reopen | Fork request `{before}` and readback, instruction-entry readback (`luna` entry carrying the seed), first prompt body (must not contain the quoted seed), side-chat docs, parent unchanged | Native fork works and leaves the parent unchanged (no orphan fork); the snapshot seed travels in the instruction channel like Claude's runtime-contract section, not as a quoted user prefix; read-only refusal text matches Claude's; side-chat layout and lifecycle identical to the Claude run | OCF-92, 155 | OC-12, OC-13 |

## 5. Scenario coverage of required categories

| Required category (user brief) | Scenarios |
| --- | --- |
| Root/child attribution | OCE-01, OCE-15 |
| Duplicate and out-of-order events | OCE-02 |
| Cancellation | OCE-03 |
| Reconnects | OCE-04 |
| Stale approvals | OCE-05, OCE-16 |
| Partial output | OCE-06 |
| Provider failure | OCE-07 |
| Images on every delivery path | OCE-08 (plus OCE-11 for remote) |
| Restart recovery | OCE-09 |
| History reopen, fork, rewind | OCE-10 |
| Remote device | OCE-11 |
| v1/v2 environments and version compatibility | OCE-00, OCE-12 |
| Models, variants, usage | OCE-13 |
| Compaction | OCE-14 |
| Native subagents vs LUNA child chats | OCE-15 |
| Approvals, questions, MCP elicitation | OCE-16 |
| Steering and queue truth | OCE-17 |
| LUNA tool delivery | OCE-18 |
| Containment of OpenCode-only state changes, discovery, background work, MCP and plugin tools, advanced workflows | OCE-19 to OCE-23 |
| Side chats (parity with Claude) | OCE-24 |

## 6. Evidence integrity checklist (per run)

- [ ] The manifest is written **before** the first send, and updated after.
- [ ] OpenCode storage is isolated (or the decision to use the user's storage is recorded and authorized).
- [ ] No credentials in frames, HTTP logs or screenshots; redaction manifest present.
- [ ] Tier named per claim; T0 and T1 never presented as T2+.
- [ ] `SHA256SUMS` over all artifacts; original and redacted hashes both recorded where redaction applied.
- [ ] QA app quit gracefully; bundle removed only after review and hashing; primary LUNA Dev reopened.
