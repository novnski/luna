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
