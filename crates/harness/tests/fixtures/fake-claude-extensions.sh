#!/bin/sh
# Fake Claude Code CLI for the CC-12 extension/hook tests
# (crates/harness/tests/claude_extensions.rs and the claude-hooks UI fixture).
#
# Frame shapes are Claude Code 2.1.286: the initialize answers are real
# isolated-handshake captures (tests/fixtures/claude/init-2.1.286-*.json), the
# SessionStart hook frames were captured live during initialization, and the
# other hook/commands_changed frames follow the installed binary's SDK schema
# (evidence: cc12-extensions/probes/sdk-schema-excerpts.txt).
#
# Every invocation appends `ARGV`, `CWD` and `STDIN` lines to
# "$CLAUDE_CONFIG_DIR/fake-log.txt" so tests can assert what was launched.

LOG="${CLAUDE_CONFIG_DIR:-/tmp}/fake-log.txt"
printf 'ARGV:' >> "$LOG"
for a in "$@"; do printf ' [%s]' "$a" >> "$LOG"; done
printf '\nCWD: %s\n' "$(pwd)" >> "$LOG"

restricted=false
for a in "$@"; do
  [ "$a" = '--restricted' ] && restricted=true
done

emit() { printf '%s\n' "$1"; }

read -r first || exit 1
printf 'STDIN: %s\n' "$first" >> "$LOG"

case "$first" in
*'"subtype":"initialize"'*)
  rid=$(printf '%s' "$first" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  printf 'INIT\n' >> "$LOG"
  canned="$CLAUDE_CONFIG_DIR/fake-initialize.json"
  [ "$restricted" = true ] && canned="$CLAUDE_CONFIG_DIR/fake-initialize-restricted.json"
  if [ -f "$canned" ]; then
    body=$(tr -d '\n' < "$canned")
  else
    # Dynamic: two built-ins plus flat user commands (unless restricted).
    cmds='{"name":"compact","description":"Clear conversation history but keep a summary in context","argumentHint":""},{"name":"output-style","description":"List output styles or switch to one","argumentHint":""}'
    if [ "$restricted" = false ]; then
      for f in "$CLAUDE_CONFIG_DIR"/commands/*.md; do
        [ -e "$f" ] || continue
        n=$(basename "$f" .md)
        d=$(sed -n 's/^description: //p' "$f" | head -n 1)
        cmds="$cmds,{\"name\":\"$n\",\"description\":\"$d (user)\",\"argumentHint\":\"\"}"
      done
    fi
    body="{\"commands\":[$cmds],\"output_style\":\"default\",\"available_output_styles\":[\"default\",\"Proactive\",\"Concise\",\"Explanatory\",\"Learning\"]}"
  fi
  emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":$body}}"
  # Keep stdin open like the real CLI until the probe closes it.
  while read -r _; do :; done
  exit 0
  ;;

*scenario:hooks*)
  # SessionStart runs before system:init (real 2.1.286 capture shape).
  emit '{"type":"system","subtype":"hook_started","hook_id":"280a2fc8-3647-4632-8142-c1d9c11b9dee","hook_name":"SessionStart:startup","hook_event":"SessionStart","uuid":"6ce8d9d7-7146-483d-8b52-3347a9524545","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"280a2fc8-3647-4632-8142-c1d9c11b9dee","hook_name":"SessionStart:startup","hook_event":"SessionStart","output":"cc12-user-session-start\n","stdout":"cc12-user-session-start\n","stderr":"","exit_code":0,"outcome":"success","uuid":"c40e0bbc-a1ba-4eb4-b598-2b9d73219a3e","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash","Read"],"cwd":"/synthetic","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-prompt","hook_name":"UserPromptSubmit","hook_event":"UserPromptSubmit","uuid":"u1","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-prompt","hook_name":"UserPromptSubmit","hook_event":"UserPromptSubmit","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","uuid":"u2","session_id":"sess-hooks"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"tool-rm","name":"Bash","input":{"command":"rm -rf build"}}]}}'
  # Blocking PreToolUse hook with live progress (cumulative output).
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-guard","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","uuid":"u3","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_progress","hook_id":"hk-guard","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","stdout":"checking command\n","stderr":"","output":"checking command\n","uuid":"u4","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_progress","hook_id":"hk-guard","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","stdout":"checking command\n","stderr":"","output":"checking command\n","uuid":"u4b","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_progress","hook_id":"hk-guard","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","stdout":"checking command\npolicy loaded\n","stderr":"","output":"checking command\npolicy loaded\n","uuid":"u5","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-guard","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","output":"checking command\npolicy loaded\nrm -rf is blocked by project policy\n","stdout":"checking command\npolicy loaded\n","stderr":"rm -rf is blocked by project policy\n","exit_code":2,"outcome":"error","uuid":"u6","session_id":"sess-hooks"}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"tool-rm","is_error":true,"content":"PreToolUse:Bash hook error: rm -rf is blocked by project policy"}]}}'
  # JSON decision with exit 0, and no hook_started (response only).
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-lint","hook_name":"PostToolUse:Edit","hook_event":"PostToolUse","output":"{\"decision\":\"block\",\"reason\":\"lint failed: 2 errors\"}\n","stdout":"{\"decision\":\"block\",\"reason\":\"lint failed: 2 errors\"}\n","stderr":"","exit_code":0,"outcome":"success","uuid":"u7","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-notify","hook_name":"Notification","hook_event":"Notification","uuid":"u8","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-notify","hook_name":"Notification","hook_event":"Notification","output":"","stdout":"","stderr":"","outcome":"cancelled","uuid":"u9","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"commands_changed","commands":[{"name":"compact","description":"Clear conversation history but keep a summary in context","argumentHint":""},{"name":"deploy-notes","description":"Draft release notes (project)","argumentHint":"<version>"}],"uuid":"u10","session_id":"sess-hooks"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"The cleanup was blocked by your project hook."}}}'
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-stop","hook_name":"Stop","hook_event":"Stop","uuid":"u11","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-stop","hook_name":"Stop","hook_event":"Stop","output":"notify-send: no display\n","stdout":"","stderr":"notify-send: no display\n","exit_code":1,"outcome":"error","uuid":"u12","session_id":"sess-hooks"}'
  # A duplicate terminal frame must not produce a second result.
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-stop","hook_name":"Stop","hook_event":"Stop","output":"late duplicate\n","stdout":"late duplicate\n","stderr":"","exit_code":0,"outcome":"success","uuid":"u13","session_id":"sess-hooks"}'
  emit '{"type":"result","subtype":"success","result":"The cleanup was blocked by your project hook.","errors":[],"usage":{"input_tokens":3,"output_tokens":9},"session_id":"sess-hooks"}'
  # SessionEnd runs during shutdown, after the final result.
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-end","hook_name":"SessionEnd","hook_event":"SessionEnd","uuid":"u14","session_id":"sess-hooks"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-end","hook_name":"SessionEnd","hook_event":"SessionEnd","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","uuid":"u15","session_id":"sess-hooks"}'
  ;;

*scenario:idle-hooks*)
  # Hooks between a root result and the wake turn (background work, idle
  # notification) must not reopen the finished turn.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Agent"],"cwd":"/synthetic","session_id":"sess-idle"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"started"}}}'
  emit '{"type":"result","subtype":"success","result":"started","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-idle"}'
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-idle-notify","hook_name":"Notification","hook_event":"Notification","uuid":"i1","session_id":"sess-idle"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-idle-notify","hook_name":"Notification","hook_event":"Notification","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","uuid":"i2","session_id":"sess-idle"}'
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-bg","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","uuid":"i3","session_id":"sess-idle"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-bg","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","uuid":"i4","session_id":"sess-idle"}'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Agent"],"cwd":"/synthetic","session_id":"sess-idle"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"woke"}}}'
  emit '{"type":"result","subtype":"success","result":"woke","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-idle"}'
  ;;

*scenario:preinit-result*)
  # A run that fails before system:init still shows its SessionStart hook.
  emit '{"type":"system","subtype":"hook_started","hook_id":"hk-early","hook_name":"SessionStart:startup","hook_event":"SessionStart","uuid":"e1","session_id":"sess-early"}'
  emit '{"type":"system","subtype":"hook_response","hook_id":"hk-early","hook_name":"SessionStart:startup","hook_event":"SessionStart","output":"warming cache\n","stdout":"warming cache\n","stderr":"","exit_code":0,"outcome":"success","uuid":"e2","session_id":"sess-early"}'
  emit '{"type":"result","subtype":"error_during_execution","errors":["Not logged in"],"usage":{"input_tokens":0,"output_tokens":0},"session_id":"sess-early"}'
  ;;

*)
  # Typed-skill and default turns: acknowledge after recording the stdin line.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Read"],"cwd":"/synthetic","session_id":"sess-skill"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"ok"}}}'
  emit '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-skill"}'
  ;;
esac
