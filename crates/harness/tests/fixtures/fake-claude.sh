#!/bin/sh
# Fake Claude Code CLI for luna-harness tests.
#
# Reads the first stream-json user line from stdin, picks a scenario from the
# prompt text, and plays a scripted stream-json transcript on stdout —
# including control-channel round-trips read back from stdin. Frame shapes
# mirror live captures from CLI 2.1.228. Driven by
# crates/harness/tests/claude.rs.

read -r first || exit 1

emit() { printf '%s\n' "$1"; }

# --- CLI 2.1.286 control-transport shapes (cc01) -----------------------------
# From the installed 2.1.286 binary's SDK schema and initialization-only
# handshakes (no model turn). system/init advertises protocol capabilities.
INIT_286='{"type":"system","subtype":"init","cwd":"/tmp","session_id":"sess-cc01","tools":["Bash","Read"],"mcp_servers":[],"model":"claude-fable-5","permissionMode":"acceptEdits","slash_commands":[],"apiKeySource":"none","claude_code_version":"2.1.286","output_style":"default","agents":[],"skills":[],"plugins":[],"capabilities":["interrupt_receipt_v1","interrupt_cancel_queued_v1","msg_lifecycle_v1"],"uuid":"a0000000-0000-4000-8000-000000000001"}'
# End the scenario with a visible failure result (quotes stripped from $1).
fail() {
  msg=$(printf '%s' "$1" | tr -d '"\\' | cut -c1-300)
  emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"errors\":[\"$msg\"],\"usage\":{\"input_tokens\":0,\"output_tokens\":0},\"session_id\":\"sess-cc01\"}"
  exit 0
}
ok() {
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"$1\",\"errors\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"terminal_reason\":\"completed\",\"session_id\":\"sess-cc01\"}"
}
uuid_of() { printf '%s\n' "$1" | sed -n 's/.*"uuid":"\([0-9a-f-]\{36\}\)".*/\1/p'; }
lifecycle() {
  emit "{\"type\":\"command_lifecycle\",\"command_uuid\":\"$1\",\"state\":\"$2\",\"uuid\":\"b0000000-0000-4000-8000-000000000002\",\"session_id\":\"sess-cc01\"}"
}
delta() {
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"$1\"}},\"session_id\":\"sess-cc01\"}"
}

case "$first" in

*scenario:cc01-unknown-controls*)
  emit "$INIT_286"
  # Words after the scenario name: inbound subtypes the test reads from the
  # compatibility ledger as ones LUNA must refuse explicitly.
  subtypes=$(printf '%s\n' "$first" | sed 's/.*scenario:cc01-unknown-controls\([^"]*\)".*/\1/')
  [ -n "$subtypes" ] || fail "no subtypes supplied"
  n=0
  for st in $subtypes; do
    n=$((n+1))
    emit "{\"type\":\"control_request\",\"request_id\":\"cr-u$n\",\"request\":{\"subtype\":\"$st\"}}"
    read -r reply || exit 1
    case "$reply" in *'"type":"control_response"'*) ;; *) fail "not a control_response for $st: $reply" ;; esac
    case "$reply" in *"\"request_id\":\"cr-u$n\""*) ;; *) fail "reply for $st has the wrong id: $reply" ;; esac
    case "$reply" in *'"subtype":"error"'*) ;; *) fail "reply for $st is not an error: $reply" ;; esac
    case "$reply" in *"Unsupported control request subtype: $st"*) ;; *) fail "reply for $st lacks the reason: $reply" ;; esac
  done
  # Noise LUNA must not answer: a duplicate id, a request without an id, a
  # stray response, a withdrawal of an unknown id, a heartbeat.
  emit '{"type":"control_request","request_id":"cr-u1","request":{"subtype":"hook_callback"}}'
  emit '{"type":"control_request","request":{"subtype":"hook_callback"}}'
  emit '{"type":"control_response","response":{"subtype":"success","request_id":"never-issued","response":{}}}'
  emit '{"type":"control_cancel_request","request_id":"never-issued"}'
  emit '{"type":"keep_alive"}'
  emit '{"type":"control_request","request_id":"cr-last","request":{"subtype":"luna_future_subtype"}}'
  read -r reply || exit 1
  case "$reply" in *'"request_id":"cr-last"'*) ;; *) fail "answered noise before cr-last: $reply" ;; esac
  delta "controls answered"
  ok "controls-ok"
  ;;

*scenario:cc01-cancel-approval*)
  emit "$INIT_286"
  emit '{"type":"control_request","request_id":"cr-perm","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"touch cc01-should-not-run"},"tool_use_id":"toolu_cc01"}}'
  # Let LUNA surface the approval, then withdraw it (e.g. the turn was
  # interrupted elsewhere, or another client answered).
  sleep 0.3
  emit '{"type":"control_cancel_request","request_id":"cr-perm"}'
  # The test answers "Allow once" late, inside this window.
  sleep 1
  emit '{"type":"control_request","request_id":"cr-after","request":{"subtype":"luna_future_subtype"}}'
  read -r reply || exit 1
  case "$reply" in *'"request_id":"cr-after"'*) ;; *) fail "late reply after withdrawal: $reply" ;; esac
  ok "cancel-ok"
  ;;

*scenario:cc01-lifecycle-steer*)
  emit "$INIT_286"
  uuid0=$(uuid_of "$first")
  [ -n "$uuid0" ] || fail "opening prompt carried no uuid"
  lifecycle "$uuid0" queued
  lifecycle "$uuid0" started
  delta "first"
  read -r steer || exit 1
  suuid=$(uuid_of "$steer")
  [ -n "$suuid" ] || fail "steer carried no uuid"
  [ "$suuid" != "$uuid0" ] || fail "uuid reused across inputs"
  lifecycle "$suuid" queued
  # A foreign uuid (another client's prompt) must not settle anything.
  lifecycle "c0000000-0000-4000-8000-000000000003" started
  lifecycle "$suuid" started
  lifecycle "$suuid" started
  delta "steered"
  lifecycle "$suuid" completed
  ok "lifecycle-ok"
  lifecycle "$uuid0" completed
  ;;

*scenario:cc01-lifecycle-refused*)
  emit "$INIT_286"
  delta "first"
  read -r steer || exit 1
  suuid=$(uuid_of "$steer")
  [ -n "$suuid" ] || fail "steer carried no uuid"
  lifecycle "$suuid" refused
  delta "after"
  ok "refused-ok"
  ;;

*scenario:cc01-lifecycle-silent*)
  emit "$INIT_286"
  delta "first"
  read -r steer || exit 1
  sleep 3
  ok "silent-ok"
  ;;

*scenario:cc01-stop-cancels-queued*)
  emit "$INIT_286"
  delta "working"
  read -r steer || exit 1
  suuid=$(uuid_of "$steer")
  read -r ctl || exit 1
  rid=$(printf '%s\n' "$ctl" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"still_queued\":[],\"cancelled\":[\"$suuid\"]}}}"
  lifecycle "$suuid" cancelled
  emit '{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["[ede_diagnostic] turn aborted (interrupt) stop_reason=null"],"usage":{"input_tokens":1,"output_tokens":1},"terminal_reason":"aborted_streaming","session_id":"sess-cc01"}'
  while read -r _; do :; done
  ;;

*scenario:cc01-interrupt-receipt*)
  trap 'printf term > sigterm-marker; exit 143' TERM
  emit "$INIT_286"
  delta "working"
  read -r ctl || exit 1
  printf '%s\n' "$ctl" > interrupt-request.json
  rid=$(printf '%s\n' "$ctl" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"still_queued\":[],\"cancelled\":[]}}}"
  emit '{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["[ede_diagnostic] turn aborted (interrupt) stop_reason=null"],"usage":{"input_tokens":1,"output_tokens":1},"terminal_reason":"aborted_streaming","session_id":"sess-cc01"}'
  # Drain: LUNA must end input before sending any signal.
  while read -r _; do :; done
  printf eof > drained-marker
  exit 0
  ;;

*scenario:cc01-auth-variants*)
  emit "$INIT_286"
  emit '{"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":529,"error":"overloaded","uuid":"d0000000-0000-4000-8000-000000000004","session_id":"sess-cc01"}'
  emit '{"type":"auth_status","isAuthenticating":false,"output":[],"error":"token expired","uuid":"d0000000-0000-4000-8000-000000000005","session_id":"sess-cc01"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]},"error":"verification_required","session_id":"sess-cc01"}'
  emit '{"type":"result","subtype":"error_luna_future_kind","is_error":true,"errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-cc01"}'
  ;;

*scenario:runtime-instructions*)
  [ "$LUNA_THREAD_ID" = 'thread-a' ] || exit 1
  appended=false
  while [ "$#" -gt 0 ]; do
    if [ "$1" = '--append-system-prompt' ]; then
      shift
      case "$1" in *'Run inside LUNA.'*) appended=true ;; esac
    fi
    shift
  done
  [ "$appended" = true ] || exit 1
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/synthetic","session_id":"sess-runtime"}'
  emit '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-runtime"}'
  ;;

*scenario:title*)
  tools_off=false
  system_set=false
  mcp_off=false
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --tools) shift; [ "$1" = "" ] && tools_off=true ;;
      --system-prompt) shift; case "$1" in "You generate session titles."*) system_set=true ;; esac ;;
      --strict-mcp-config) mcp_off=true ;;
      --dangerously-skip-permissions) exit 1 ;;
    esac
    shift
  done
  [ "$tools_off" = true ] && [ "$system_set" = true ] && [ "$mcp_off" = true ] || exit 1
  emit '{"type":"control_request","request_id":"title-tool","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch should-not-exist"}}}'
  read -r response || exit 1
  case "$response" in *'"behavior":"deny"'*) ;; *) exit 1 ;; esac
  emit '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Fix Login Flow"}}}'
  emit '{"type":"result","subtype":"success","result":"Fix Login Flow","usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-title"}'
  ;;


*scenario:happy*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash","Read"],"cwd":"/tmp","session_id":"sess-1"}'
  # Re-emitted init mid-run (background-task wakeup): must be deduped.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash","Read"],"cwd":"/tmp","session_id":"sess-1"}'
  # Post-init system subtypes the 2.1.x CLI emits (thinking accounting,
  # session state): must be tolerated and dropped.
  emit '{"type":"system","subtype":"thinking_tokens","tokens":12}'
  emit '{"type":"system","subtype":"session_state_changed","state":"running"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"pondering"}}}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}}'
  # Subagent frames (parent_tool_use_id set): tagged, never in the parent feed.
  emit '{"type":"stream_event","parent_tool_use_id":"sub-1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"SUBAGENT"}}}'
  emit '{"type":"assistant","parent_tool_use_id":"sub-1","message":{"content":[{"type":"tool_use","id":"sub-tool","name":"Bash","input":{"command":"echo sub"}}]}}'
  emit '{"type":"user","parent_tool_use_id":"sub-1","message":{"content":[{"type":"tool_result","tool_use_id":"sub-tool","is_error":false}]}}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","id":"tool-2","name":"mcp__linear__search","input":{"q":"bug"}}]}}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":false},{"type":"tool_result","tool_use_id":"tool-2","is_error":true}]}}'
  # Informational rate-limit status: stays quiet.
  emit '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}'
  emit '{"type":"result","subtype":"success","result":"done!","errors":[],"usage":{"input_tokens":10,"output_tokens":20},"session_id":"sess-1","total_cost_usd":0.01}'
  ;;

*scenario:wake*)
  # Eager-done + wake, the live-verified 2.1.228 background-subagent shape:
  # the parent turn settles with result #1 while the subagent still runs;
  # tagged subagent traffic continues; the CLI then wakes with a second init
  # (SAME session id) and settles the wake turn with result #2.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-wake"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_agent","name":"Agent","input":{"description":"background task","run_in_background":true}}]}}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"LAUNCHED"}}}'
  emit '{"type":"result","subtype":"success","result":"LAUNCHED","errors":[],"usage":{"input_tokens":5,"output_tokens":5},"session_id":"sess-wake"}'
  # Background subagent interior, after the eager done.
  emit '{"type":"stream_event","parent_tool_use_id":"toolu_agent","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"sub working"}}}'
  emit '{"type":"assistant","parent_tool_use_id":"toolu_agent","message":{"content":[{"type":"tool_use","id":"sub-t1","name":"Bash","input":{"command":"sleep 1"}}]}}'
  emit '{"type":"user","parent_tool_use_id":"toolu_agent","message":{"content":[{"type":"tool_result","tool_use_id":"sub-t1","is_error":false}]}}'
  # The wake turn: re-init (deduped), untagged output, second result.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-wake"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"subagent finished"}}}'
  emit '{"type":"result","subtype":"success","result":"wrapped up","errors":[],"usage":{"input_tokens":3,"output_tokens":3},"session_id":"sess-wake"}'
  ;;

*scenario:askuser*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-ask"}'
  # A plain tool permission request: must be auto-allowed.
  emit '{"type":"control_request","request_id":"cr-0","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
  read -r resp0 || exit 1
  case "$resp0" in
  *'"request_id":"cr-0"'*'"behavior":"allow"'*) ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["bash tool was not allowed"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
    exit 0
    ;;
  esac
  # AskUserQuestion: must be intercepted and answered via updatedInput.answers.
  emit '{"type":"control_request","request_id":"cr-1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"header":"Choice","question":"Pick one","options":["A","B"],"multiSelect":false}]}}}'
  read -r resp1 || exit 1
  case "$resp1" in
  *'"behavior":"allow"'*)
    case "$resp1" in
    *'"Pick one":"B"'*)
      emit '{"type":"result","subtype":"success","result":"answered","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
      ;;
    *)
      emit '{"type":"result","subtype":"error_during_execution","errors":["answers missing from updatedInput"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
      ;;
    esac
    ;;
  *)
    emit '{"type":"result","subtype":"error_during_execution","errors":["AskUserQuestion was denied"],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-ask"}'
    ;;
  esac
  ;;

*scenario:steer-accepted-only*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-steer"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"first"}}}'
  read -r steer || exit 1
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"accepted-without-incorporation"}}}'
  emit '{"type":"result","subtype":"success","result":"accepted","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-steer"}'
  ;;

*scenario:steer*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-steer"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"first"}}}'
  # The queued steering user line, applied at "the step boundary" (here: now).
  read -r steer || exit 1
  content=$(printf '%s\n' "$steer" | sed 's/.*"content":"\([^"]*\)".*/\1/')
  emit "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"$content\"}]}}"
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"steered:$content\"}}}"
  emit '{"type":"result","subtype":"success","result":"steered","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-steer"}'
  ;;

*scenario:interrupt*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-int"}'
  # Wedge without reading stdin — forces the SIGTERM escalation path.
  exec sleep 30
  ;;

*'"subtype":"initialize"'*)
  # Command discovery: the initialize control request arrives as the FIRST
  # stdin line (no user message ever follows). Shape mirrors 2.1.228's
  # control_response: commands under response.response.
  rid=$(printf '%s\n' "$first" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
  emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{\"commands\":[{\"name\":\"review\",\"description\":\"Review a pull request\",\"argumentHint\":\"[pr number]\"},{\"name\":\"compact\",\"description\":\"Compact the conversation\",\"argumentHint\":\"\"},{\"name\":\"\",\"description\":\"nameless: dropped\"}],\"models\":[{\"value\":\"default\",\"resolvedModel\":\"claude-fable-5\"},{\"value\":\"claude-fable-5[1m]\",\"resolvedModel\":\"claude-fable-5\",\"displayName\":\"Fable\",\"supportsEffort\":true,\"supportedEffortLevels\":[\"low\",\"medium\",\"high\",\"xhigh\",\"max\"]},{\"value\":\"claude-fresh-7\",\"resolvedModel\":\"claude-fresh-7\",\"displayName\":\"Fresh 7\",\"description\":\"Newly available\",\"supportsEffort\":true,\"supportedEffortLevels\":[\"low\",\"medium\",\"high\"]}],\"output_style\":\"default\"}}}"
  # Stay alive until the driver tears us down, like the real CLI would.
  exec sleep 30
  ;;

*scenario:error*)
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-err"}'
  # Terse assistant-level error code with no content.
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]},"error":"rate_limit"}'
  # Hard-rejected claude.ai usage window.
  emit '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour"}}'
  # Result error with an EMPTY errors array: needs fallback wording.
  emit '{"type":"result","subtype":"error_max_turns","errors":[],"usage":{"input_tokens":1,"output_tokens":2},"session_id":"sess-err"}'
  ;;

*)
  emit '{"type":"result","subtype":"error_during_execution","errors":["unknown scenario"],"usage":{"input_tokens":0,"output_tokens":0},"session_id":"sess-x"}'
  ;;
esac
