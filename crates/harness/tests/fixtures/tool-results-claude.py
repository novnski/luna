#!/usr/bin/env python3
"""Deterministic Claude Code CLI peer for tool-result fidelity (E28). No model calls.

Frame shapes follow the Claude Code 2.1.286 SDK schema and emitter extracted from the
installed bundle (see evidence/cc05-tools/schema-excerpts.txt):
- user frames carry `tool_use_result` = the tool's full Output object, or for MCP calls with
  metadata `{content, _meta?, structuredContent?, resourceLinks?}`, plus `tool_result_meta`.
- MCP content arrives rewritten for the model (images as Anthropic base64 blocks, resources
  and resource links as text lines); `resourceLinks` keeps the original link blocks.

Scenarios (picked from the first stdin user line):
- `scenario:rich-results` (default): every tool family, a child Agent with a colliding id,
  duplicates, a multi-result frame, unknown blocks, large output and structured output.
- `scenario:structured-only`: a result frame whose `structured_output` has no matching call.
Title runs (`--tools ""`) get a one-line title.
"""
import json
import sys
import uuid

SESSION = "sess-tool-results"
PNG = "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEUlEQVR4nGPQqLiDFTEMLQkA8eRfATA3bycAAAAASUVORK5CYII="


def emit(value):
    value.setdefault("session_id", SESSION)
    value.setdefault("uuid", str(uuid.uuid4()))
    print(json.dumps(value, ensure_ascii=False), flush=True)


def assistant(blocks, parent=None):
    emit({"type": "assistant", "parent_tool_use_id": parent,
          "message": {"id": "msg_" + uuid.uuid4().hex[:12], "type": "message", "role": "assistant",
                      "model": "claude-haiku-4-5-20251001", "content": blocks,
                      "stop_reason": None, "usage": {"input_tokens": 3, "output_tokens": 2}}})


def call(tool_id, name, tool_input, parent=None):
    assistant([{"type": "tool_use", "id": tool_id, "name": name, "input": tool_input}], parent)


def result(tool_id, content=None, tool_use_result=None, is_error=False, parent=None, meta=None,
           extra_blocks=None):
    block = {"type": "tool_result", "tool_use_id": tool_id, "is_error": is_error}
    if content is not None:
        block["content"] = content
    frame = {"type": "user", "parent_tool_use_id": parent,
             "message": {"role": "user", "content": [block] + (extra_blocks or [])},
             "timestamp": "2026-10-01T12:00:00.000Z"}
    if tool_use_result is not None:
        frame["tool_use_result"] = tool_use_result
    if meta is not None:
        frame["tool_result_meta"] = meta
    emit(frame)
    return frame


def bash_output(stdout, stderr=""):
    return {"stdout": stdout, "stderr": stderr, "interrupted": False, "isImage": False,
            "noOutputExpected": False}


def init():
    emit({"type": "system", "subtype": "init", "cwd": "/fixture", "model": "claude-haiku-4-5-20251001",
          "permissionMode": "acceptEdits", "apiKeySource": "none", "claude_code_version": "2.1.286",
          "tools": ["Bash", "Read", "Edit", "Write", "Glob", "Grep", "NotebookEdit", "Agent",
                    "StructuredOutput", "mcp__fixture__rich", "mcp__fixture__noop",
                    "mcp__computer-use__screenshot"],
          "mcp_servers": [{"name": "fixture", "status": "connected"},
                          {"name": "computer-use", "status": "connected"}],
          "slash_commands": [], "output_style": "default", "skills": [], "plugins": []})


def finish(structured=None):
    frame = {"type": "result", "subtype": "success", "is_error": False, "duration_ms": 10,
             "duration_api_ms": 8, "num_turns": 3, "result": "Fixture prose result.",
             "stop_reason": "end_turn", "total_cost_usd": 0.0,
             "usage": {"input_tokens": 11, "output_tokens": 7},
             "modelUsage": {"claude-haiku-4-5-20251001": {"inputTokens": 11, "outputTokens": 7,
                                                          "contextWindow": 200000}},
             "permission_denials": []}
    if structured is not None:
        frame["structured_output"] = structured
    emit(frame)


def rich_results():
    init()
    # Bash: stdout + stderr; lossless arguments beyond `command`.
    call("toolu_bash", "Bash", {"command": "printf fixture", "description": "BASH_DESCRIPTION_ARG",
                                "timeout": 120000, "run_in_background": False})
    stdout = "BASH_STDOUT_MARKER first line of the command output"
    stderr = "BASH_STDERR_MARKER warning emitted on standard error"
    bash = result("toolu_bash", stdout + "\n" + stderr, bash_output(stdout, stderr))
    # An identical replay of the same frame must not produce a second result.
    emit(dict(bash, uuid=str(uuid.uuid4())))

    # Read with pagination arguments; canonical text is the numbered block.
    call("toolu_read", "Read", {"file_path": "/fixture/notes.txt", "offset": 10, "limit": 2})
    raw = "READ_LINE_TEN\nREAD_LINE_ELEVEN"
    result("toolu_read", "    10→READ_LINE_TEN\n    11→READ_LINE_ELEVEN",
           {"type": "text", "file": {"filePath": "/fixture/notes.txt", "content": raw,
                                     "numLines": 2, "startLine": 10, "totalLines": 200}})

    # Edit: native structuredPatch; originalFile is a large pre-image.
    call("toolu_edit", "Edit", {"file_path": "/fixture/a.rs", "old_string": "before();",
                                "new_string": "after();", "replace_all": True})
    original = "fn main() {\n    before();\n}\n// ORIGINAL_FILE_MARKER\n"
    result("toolu_edit",
           "The file /fixture/a.rs has been updated. Here's the result of running `cat -n` on a snippet of the edited file:\n     1→fn main() {\n     2→    after();",
           {"filePath": "/fixture/a.rs", "oldString": "before();", "newString": "after();",
            "originalFile": original,
            "structuredPatch": [{"oldStart": 1, "oldLines": 3, "newStart": 1, "newLines": 3,
                                 "lines": [" fn main() {", "-    before();", "+    after();", " }"]}],
            "userModified": False, "replaceAll": True})

    # Edit held for review: no diff may be shown.
    call("toolu_staged", "Edit", {"file_path": "/fixture/b.rs", "old_string": "x", "new_string": "y"})
    result("toolu_staged", "The edit to /fixture/b.rs is staged for review.",
           {"filePath": "/fixture/b.rs", "oldString": "x", "newString": "y", "originalFile": "x\n",
            "structuredPatch": [{"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                                 "lines": ["-x", "+y"]}],
            "userModified": False, "replaceAll": False, "staged": True})

    # Malformed patch lines: no fabricated diff.
    call("toolu_badpatch", "Edit", {"file_path": "/fixture/c.rs", "old_string": "p", "new_string": "q"})
    result("toolu_badpatch", "The file /fixture/c.rs has been updated.",
           {"filePath": "/fixture/c.rs", "oldString": "p", "newString": "q", "originalFile": "p\n",
            "structuredPatch": [{"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                                 "lines": ["p", "q"]}],
            "userModified": False, "replaceAll": False})

    # Write create: the created content is the diff.
    body = "CREATE_BODY_MARKER line one\nline two\n"
    call("toolu_write", "Write", {"file_path": "/fixture/new.txt", "content": body})
    result("toolu_write", "File created successfully at: /fixture/new.txt",
           {"type": "create", "filePath": "/fixture/new.txt", "content": body,
            "structuredPatch": [], "originalFile": None})

    # NotebookEdit: cell-level native old/new source.
    call("toolu_nb", "NotebookEdit", {"notebook_path": "/fixture/n.ipynb", "cell_id": "c1",
                                      "new_source": "print(2)", "edit_mode": "replace"})
    result("toolu_nb", "Updated cell c1 with print(2)",
           {"new_source": "print(2)", "old_source": "print(1)", "cell_id": "c1",
            "cell_type": "code", "language": "python", "edit_mode": "replace"})

    # Grep with options the typed call drops.
    call("toolu_grep", "Grep", {"pattern": "TODO", "path": "/fixture", "glob": "*.rs",
                                "output_mode": "content", "-n": True, "-i": True, "head_limit": 5})
    result("toolu_grep", "/fixture/a.rs:3:// TODO GREP_MATCH_MARKER",
           {"mode": "content", "numFiles": 1, "filenames": [],
            "content": "/fixture/a.rs:3:// TODO GREP_MATCH_MARKER", "numLines": 1})

    # Mixed MCP result: ordered text, image, resource text and resource link, plus metadata.
    call("toolu_mcp", "mcp__fixture__rich", {"query": "MCP_ARGUMENT_MARKER"})
    mcp_content = [
        {"type": "text", "text": "MCP_TEXT_MARKER summary of the call"},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": PNG}},
        {"type": "text", "text": "[Resource from fixture at file:///fixture/res.txt] RESOURCE_TEXT_MARKER"},
        {"type": "text", "text": "[Resource link: report] file:///fixture/report.pdf (Quarterly report)"},
    ]
    result("toolu_mcp", mcp_content,
           {"content": mcp_content,
            "structuredContent": {"rows": [{"id": 1, "status": "ok"}], "marker": "STRUCTURED_MARKER"},
            "_meta": {"fixture/trace": "META_MARKER"},
            "resourceLinks": [{"type": "resource_link", "uri": "file:///fixture/report.pdf",
                               "name": "report", "mimeType": "application/pdf",
                               "description": "Quarterly report"}]})

    # Empty MCP result (received, but empty) versus unavailable output below.
    call("toolu_empty", "mcp__fixture__noop", {})
    result("toolu_empty", [], [])

    # Computer-use screenshot bytes are never retained.
    call("toolu_cu", "mcp__computer-use__screenshot", {"display": 1})
    screenshot = [{"type": "image", "source": {"type": "base64", "media_type": "image/png",
                                               "data": "SCREENSHOT_SECRET" + PNG}}]
    result("toolu_cu", screenshot, screenshot)

    # Permission-denied call: error plus non-execution evidence.
    call("toolu_denied", "Bash", {"command": "rm -rf /tmp/fixture"})
    denied = "Permission to use Bash with command rm -rf /tmp/fixture has been denied."
    result("toolu_denied", denied, "Error: " + denied, is_error=True,
           meta=[{"id": "toolu_denied", "non_execution_kind": "permission-rule"}])

    # Future tool with an unknown content block type.
    call("toolu_future", "FutureTool", {"payload": "FUTURE_ARGUMENT_MARKER"})
    result("toolu_future", [{"type": "text", "text": "FUTURE_TEXT_MARKER"},
                            {"type": "future_widget", "payload": "FUTURE_BLOCK_MARKER", "nested": {"a": 1}}])

    # Large output above every synced/preview limit.
    large = "LARGE_HEAD\n" + "é" * 40000 + "\nLARGE_TAIL"
    call("toolu_large", "Bash", {"command": "cat big.txt"})
    result("toolu_large", large, bash_output(large))

    # A result with neither content nor tool_use_result: unavailable.
    call("toolu_unavailable", "Bash", {"command": "true"})
    result("toolu_unavailable")

    # Two results in one frame: the frame-level envelope cannot be attributed.
    assistant([{"type": "tool_use", "id": "toolu_glob_a", "name": "Glob", "input": {"pattern": "*.a", "path": "/fixture/a"}},
               {"type": "tool_use", "id": "toolu_glob_b", "name": "Glob", "input": {"pattern": "*.b"}}])
    emit({"type": "user", "parent_tool_use_id": None,
          "message": {"role": "user", "content": [
              {"type": "tool_result", "tool_use_id": "toolu_glob_a", "content": "GLOB_A_RESULT"},
              {"type": "tool_result", "tool_use_id": "toolu_glob_b", "content": "GLOB_B_RESULT"}]},
          "tool_use_result": {"filenames": ["MISATTRIBUTED_ENVELOPE"], "numFiles": 1,
                              "durationMs": 1, "truncated": False}})

    # Malformed envelope shape (a number) must not break the result.
    call("toolu_weird", "Glob", {"pattern": "*.weird"})
    result("toolu_weird", "No files found", 42)

    # Foreground child: its tool id collides with the parent's Bash id.
    call("toolu_agent", "Agent", {"description": "Child check", "prompt": "Run the child check",
                                  "subagent_type": "general-purpose"})
    call("toolu_bash", "Bash", {"command": "echo child", "description": "CHILD_DESCRIPTION_ARG"},
         parent="toolu_agent")
    child_out = "CHILD_OUTPUT_MARKER child command output CHILD_TAIL"
    result("toolu_bash", child_out, bash_output(child_out), parent="toolu_agent")
    result("toolu_agent", [{"type": "text", "text": "CHILD_REPORT_MARKER"}],
           {"status": "completed", "prompt": "Run the child check", "agentId": "a1",
            "content": [{"type": "text", "text": "CHILD_REPORT_MARKER"}],
            "totalDurationMs": 10, "totalTokens": 20, "totalToolUseCount": 1})

    # Schema-validated output: the StructuredOutput tool, then the result frame.
    structured = {"verdict": "pass", "score": 3}
    call("toolu_so", "StructuredOutput", structured)
    result("toolu_so", "Structured output provided successfully",
           "Structured output provided successfully")
    assistant([{"type": "text", "text": "All fixture tools ran."}])
    finish(structured)


def structured_only():
    init()
    assistant([{"type": "text", "text": "Structured answer follows."}])
    finish({"answer": "SYNTHETIC_STRUCTURED_MARKER"})


def main():
    argv = sys.argv[1:]
    first = sys.stdin.readline()
    if not first:
        return 1
    request = json.loads(first)
    if request.get("type") == "control_request" and request.get("request", {}).get("subtype") == "initialize":
        # Discovery handshake: answer like 2.1.286 (no commands/models needed here)
        # and stay alive until the driver closes stdin, like the real CLI.
        emit({"type": "control_response", "response": {
            "subtype": "success", "request_id": request.get("request_id", ""),
            "response": {"commands": [], "models": [], "output_style": "default"}}})
        sys.stdin.read()
        return 0
    if "--tools" in argv and argv[argv.index("--tools") + 1:argv.index("--tools") + 2] == [""]:
        emit({"type": "result", "subtype": "success", "result": "Fixture Title",
              "usage": {"input_tokens": 1, "output_tokens": 1}})
        return 0
    if "scenario:structured-only" in first:
        structured_only()
    else:
        rich_results()
    return 0


if __name__ == "__main__":
    sys.exit(main())
