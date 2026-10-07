# Tool output retention and live activity

Codex command, file diff, web result and MCP output is retained in the owning host's normalized run journal before LUNA limits previews. This preserves the bytes and typed payloads LUNA actually receives; provider truncation, missing notifications and existing RPC/message/memory limits still apply. Older journals that were already truncated cannot recover their missing output.

The synced document keeps a completed-output summary of up to 160 characters plus its truncation marker or a live tail of at most 2 KiB, plus byte counts, diff statistics and lazy host references. Private MCP/dynamic/computer-use results contribute only safe type/count metadata to document and event previews. Inputs remain sanitized. Full results become available when the user opens the settled tool's output (a tool left unresolved by an interrupted or errored run counts as settled once the chat has no live run); Copy keeps the complete fetched payload even when the rendered detail is limited to 400 wrapped rows.

`ToolActivity { id, kind, text }` adds stdout, stdin and patch progress. Its item ID shares the call/result owner. Patch activity carries serialized `ToolDiff`. Activity after a steering boundary updates the original assistant entry, and newly discovered per-file chips inherit the original patch's entry. File changes start with the same per-file IDs used by completion. The current Codex contract sorts change arrays by path; the retained IDs follow that native item/order contract.

Full command retrieval includes the authoritative completion text and a separately labeled history of received stdout/stdin. This can duplicate stdout deliberately: completion text and streamed notifications are separate received sources, and LUNA does not discard one when the other differs or is absent. Duplicate completion metadata does not erase live history. Subagent detail is scoped to its original spawn/document so a child's native item ID cannot overwrite its parent's output; remote child detail routes to the parent chat's owning host.

All dynamic tools have visible lifecycle, including LUNA cross-task calls. Their arguments and returned task bodies stay out of synced previews. Rich MCP/dynamic results use a `lunaToolOutput` JSON envelope in the host journal. The detail fallback displays text, resource descriptions, structured content, errors and app resource URIs. Image/audio/binary data and metadata remain in the full result for Copy; this feature does not render interactive MCP Apps or add an audio player. Computer-use results are the exception: they carry screenshots of the user's desktop, so the adapter keeps their typed shape, text, structured content and errors but replaces encoded image/audio `data` and resource `blob` fields with a byte count (`dataOmitted`/`blobOmitted`) before anything reaches the event stream or journal. Unknown items receive a bounded type/field-name diagnostic without copying arbitrary values. Known non-tool Codex items (user/agent messages, reasoning, plan, context compaction, hook prompts and review-mode markers) produce no chip.

## Repeatable offline integration validation

The failure matrix was recorded before implementation. `crates/engine/tests/tool_output.rs` drives a deterministic Python app-server peer through the actual Codex adapter, engine, document fold, journal, lazy RPC and engine restart. It covers UTF-8 command/diff/web/MCP payloads over 64 KiB, stdout/stdin before and after steer, a patch with initially empty changes and late fan-out, duplicate completion, sanitized mixed/private output, dynamic lifecycle, unknown items, and colliding parent/child native IDs.

Run from the coordinator's validated checkout, with the shared build cache and build guard:

```sh
LUNA_TOOL_OUTPUT_EVIDENCE=/absolute/path/to/evidence/tool-output/offline \
  scripts/cargo.sh test --locked -p luna-engine --test tool_output -- --nocapture
```

The artifact directory contains `doc.json`, `preview-events.json`, `journal.jsonl`, `command.txt`, `patch.json`, `mcp.json`, `child-doc.json`, `child-output.txt` and `result.json`. Hash the files after reviewing them. The fixture is offline protocol/engine evidence. It does not prove real-provider timing, media validity or native UI rendering.

For native corroboration, run one disposable ordinary repository/task with the coordinator-owned LUNA Dev app. Ask Codex to run a harmless delayed command that prints a distinctive head, more than 64 KiB of UTF-8 text and a tail; steer after live output appears. Verify the original chip updates, finishes at the original call site and retrieves the tail after reopening. Exercise an add/edit patch and a mixed-content MCP fixture, then open full detail and Copy. Save native screenshots, raw provider frames/version, document/RPC readback, the host journal and hashes. Preserve the primary app/data/cache and use no Git worktrees.

## Claude Code results (`kind: "native"`)

The Claude adapter projects every received `tool_result` once into a `lunaToolOutput` envelope with `kind: "native"` (`crates/harness/src/claude/tool_results.rs`; display helpers in `crates/doc/src/native_output.rs`). It keeps ordered typed content (text, image with MIME and base64, document, search result, unsupported blocks with their type and field names), MCP `structuredContent`/`_meta`/`resourceLinks` from the 2.1.x `tool_use_result` spread, the tool's native Output object without fields that repeat the content, diff or call arguments (listed under `deduplicated`; large pre-images under `omitted` with byte counts), the complete call arguments and any `tool_result_meta` non-execution reason. A result with no content and no `tool_use_result` stays unavailable (`output: None`); a received empty result is `empty: true`. Diffs come only from native `structuredPatch`, Write create or before/after content and NotebookEdit cell sources. Computer-use image bytes are replaced by `dataOmitted`. Documents and live previews receive only the type/count summary or, for typed built-in tools, a bounded summary of the text blocks.

Offline validation uses a 2.1.286-shaped peer (`crates/harness/tests/fixtures/tool-results-claude.py`):

```sh
scripts/cargo.sh test --locked -p luna-harness --test claude_tool_results
LUNA_CC05_EVIDENCE=/absolute/evidence/dir scripts/cargo.sh test --locked -p luna-engine --test claude_tool_results
```

The ignored `live_capture_replay_projects_every_received_result` test replays frames captured from the real CLI (`CC05_REPLAY_FRAMES=/abs/frames.jsonl`).

## OpenCode results (`kind: "native"`, `provider: "opencode"`)

The OpenCode adapter projects every terminal tool frame of the 2.x wire (`session.tool.success` / `session.tool.failed`) once into the same envelope (`crates/harness/src/opencode/tool_output.rs`); nothing is capped in the adapter. It keeps the ordered native content (`text`; a `file` item becomes `image`/`document` with `mimeType`, base64 `data` and decoded `bytes`, or a `resource_link` for a non-`data:` URI; unknown items become `unsupported` with their type and field names), the native `metadata` (exit code, signal, timeout, `truncated`, the managed `outputPath`, Code Mode `toolCalls`; oversized fields keep only a byte count), the complete call arguments under `input`, a bounded `resultState` and `providerExecuted`. A failed call's error message is its first text block, with `error: {type, message}` beside it; partial content or live output after a failure is kept and the envelope says `partial`. A received result whose text is empty is `empty: true`; a call with no terminal frame has no result at all. Diffs come only from native evidence: `edit`/`patch` `metadata.files` patches (reduced to their hunks) and a `write` whose receipt says it created the file (its content is the new text); an overwrite has no before-image and no diff.

Tool names map onto the shared chips: `shell` → `Exec`, `read`/`write`/`edit`/`glob`/`grep`/`webfetch`/`websearch` → their typed calls (`grep`'s `include` filter stays in the arguments), `patch` → one chip per file operation (`<call>`, `<call>#1`, … in patch order, typed like Codex's file changes), `subagent` → the `Agent: <description>` spawn name, `opencode_read_mcp_resource` → `Mcp`, MCP `<server>_<tool>` → `Mcp{server, tool}` split against `GET /api/mcp` server names (longest match), and `skill`/`question`/`execute`/other `opencode_*`/plugin tools → generic cards with their input preserved. An unknown tool name gets one bounded diagnostic (name and argument field names only).

OpenCode 2.0.22's `shell` streams nothing over the bus and returns only the last 50 KiB / 2,000 lines. On its first `session.tool.progress {shellID}` the adapter polls `GET /api/shell/{id}/output?cursor=` and feeds new bytes back through the run's own event loop as `ToolActivity{Stdout}` (the live tail, sharing the call's chip id). At the terminal frame the loop stops the poller and drains the rest before emitting the result, so the host journal holds the complete combined output and no activity follows its result or `Done`. The envelope's `liveOutput` records `byteLength`, `sha256`, `complete` (or the `reason`: shell removed after Stop, the 16 MiB cap, a cursor gap) and the native preview block it replaces; the detail view shows "Full shell output" from the journal's live activity in place of that preview, or "Live output received" when the capture is incomplete. Other tool progress becomes a bounded `ToolActivity{Progress}` status line (web search provider, Code Mode nested calls); streamed tool arguments (`session.tool.input.delta`) are empty-reasoning liveness heartbeats, as Claude's input deltas are.

Real-binary validation (OpenCode 2.0.22, isolated HOME/XDG) drives the actual driver, host `RunJournal` and doc fold:

```sh
scripts/cargo.sh build --locked -p luna-engine --example opencode_tool_output_probe
PROBE_WORKSPACE=/abs/ws PROBE_MARKERS=HEAD,TAIL \
  target/debug/examples/opencode_tool_output_probe ~/.opencode/bin/opencode opencode/big-pickle /abs/out "<prompt>"
```

It writes `events.jsonl`, the journal, `doc-parts.json`, each tool's journal retrieval and rendered detail, and `summary.json`.
