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

# ── CC-02 helpers (2.1.286 shapes from the CLI's embedded SDK schema) ──────
# `uuid_of` (above) reads the native client uuid LUNA stamps on a stdin line.
# An echo-capable init (claude_code_version >= 2.1.286).
init286() {
  emit "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"claude-fable-5\",\"tools\":[\"Bash\"],\"cwd\":\"/tmp\",\"session_id\":\"$1\",\"claude_code_version\":\"2.1.286\",\"capabilities\":[\"interrupt_receipt_v1\",\"msg_lifecycle_v1\"],\"permissionMode\":\"acceptEdits\",\"output_style\":\"default\",\"uuid\":\"init-$1\"}"
}
# stamped_text SESSION UUID TEXT FRAME_UUID — a turn's first stamped stream event.
stamped_text() {
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"$3\"}},\"user_message_uuid\":\"$2\",\"user_message_uuids\":[\"$2\"],\"uuid\":\"$4\",\"session_id\":\"$1\"}"
}
plain_text() {
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"$2\"}},\"uuid\":\"$3\",\"session_id\":\"$1\"}"
}
# own_result SESSION UUID RESULT_UUID — a result echoing one LUNA send.
own_result() {
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1,\"result\":\"ok\",\"errors\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":6},\"session_id\":\"$1\",\"user_message_uuid\":\"$2\",\"user_message_uuids\":[\"$2\"],\"uuid\":\"$3\"}"
}
# wake_result SESSION RESULT_UUID — a turn Claude started itself (origin only).
wake_result() {
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1,\"result\":\"wake\",\"errors\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":4},\"session_id\":\"$1\",\"origin\":{\"kind\":\"task-notification\"},\"queued_turn_count\":1,\"uuid\":\"$2\"}"
}
bg_notification() {
  emit "{\"type\":\"system\",\"subtype\":\"task_notification\",\"task_id\":\"bg-old\",\"tool_use_id\":\"toolu_old_bash\",\"status\":\"completed\",\"output_file\":\"/tmp/bg-old.out\",\"summary\":\"old build finished\",\"uuid\":\"tn-$2\",\"session_id\":\"$1\"}"
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
  # Wait until LUNA surfaced the approval (the test's input bridge drops a
  # marker in the cwd), then withdraw it (e.g. the turn was interrupted
  # elsewhere, or another client answered).
  waited=0
  while [ ! -f surfaced ]; do
    sleep 0.05
    waited=$((waited+1))
    [ "$waited" -lt 200 ] || fail "approval never surfaced"
  done
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
      --forward-subagent-text) exit 1 ;;
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


*scenario:workflow*)
  # Native dynamic workflow (task_type local_workflow) plus an experimental
  # teammate and a CC-08-owned shell task. Shapes follow the 2.1.286 binary's
  # SDK schema (task_started/task_updated/task_notification) and its workflow
  # emitter (task_progress.workflow_progress, logs filtered). The launching
  # turn settles first; the run then reports while the session is parked and
  # ends in a wake turn — the 2.1.228 background-task shape.
  S='"session_id":"sess-wf"'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Workflow","Agent","Bash"],"cwd":"/tmp","session_id":"sess-wf"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_wf","name":"Workflow","input":{"script":"export const meta = { name: \"audit-routes\", description: \"Audit routes\" }\nphase(\"Discover\")\nawait agent(\"List routes\", { label: \"list routes\" })"}}]}}'
  emit '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"wf1","task_type":"local_workflow","description":"Audit routes"}],"uuid":"u-btc-1",'"$S"'}'
  emit '{"type":"system","subtype":"task_started","task_id":"wf1","tool_use_id":"toolu_wf","description":"Audit routes","task_type":"local_workflow","workflow_name":"audit-routes","prompt":"","uuid":"u-ts-1",'"$S"'}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_wf","content":"Workflow running in background (task wf1).","is_error":false}]}}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Started the audit workflow."}]}}'
  emit '{"type":"result","subtype":"success","result":"Started the audit workflow.","errors":[],"usage":{"input_tokens":5,"output_tokens":5},'"$S"'}'
  P1='{"type":"workflow_phase","index":1,"title":"Discover","kind":"agent"}'
  A1RUN='{"type":"workflow_agent","index":1,"label":"list routes","phaseIndex":1,"phaseTitle":"Discover","model":"claude-haiku-4-5","state":"progress","queuedAt":1790000000000,"promptPreview":"List routes","lastProgressAt":1790000001000}'
  PROG1='{"type":"system","subtype":"task_progress","task_id":"wf1","tool_use_id":"toolu_wf","description":"Discover: list routes","usage":{"total_tokens":1200,"tool_uses":2,"duration_ms":4000},"last_tool_name":"list routes","summary":"Audit routes","workflow_progress":['"$P1,$A1RUN"'],"uuid":"u-tp-1",'"$S"'}'
  emit "$PROG1"
  emit "$PROG1"
  emit '{"type":"system","subtype":"task_progress","task_id":"wf1","tool_use_id":"toolu_wf","description":"Discover: list routes","usage":{"total_tokens":1500,"tool_uses":3,"duration_ms":6000},"last_tool_name":"list routes","summary":"Audit routes","uuid":"u-tp-2",'"$S"'}'
  A1DONE='{"type":"workflow_agent","index":1,"label":"list routes","phaseIndex":1,"phaseTitle":"Discover","agentId":"ag-1","model":"claude-haiku-4-5","state":"done","tokens":1400,"toolCalls":3,"resultPreview":"12 routes","lastProgressAt":1790000002000}'
  P2='{"type":"workflow_phase","index":2,"title":"Audit","kind":"pipeline"}'
  A2='{"type":"workflow_agent","index":2,"label":"audit a.ts","phaseIndex":2,"phaseTitle":"Audit","model":"claude-haiku-4-5","state":"progress","tokens":300,"toolCalls":1,"lastProgressAt":1790000003000}'
  A3='{"type":"workflow_agent","index":3,"label":"audit b.ts","phaseIndex":2,"phaseTitle":"Audit","state":"error","blocked":true,"error":"[audit b.ts] blocked by safety classifier: writes outside the project","lastProgressAt":1790000003500}'
  emit '{"type":"system","subtype":"task_progress","task_id":"wf1","tool_use_id":"toolu_wf","description":"Audit: audit a.ts","usage":{"total_tokens":3100,"tool_uses":5,"duration_ms":9000},"last_tool_name":"audit a.ts","summary":"Audit routes","workflow_progress":['"$P1,$A1DONE,$P2,$A2,$A3"'],"uuid":"u-tp-3",'"$S"'}'
  emit '{"type":"system","subtype":"task_updated","task_id":"wf1","patch":{"status":"paused"},"uuid":"u-tu-1",'"$S"'}'
  emit '{"type":"system","subtype":"task_updated","task_id":"wf1","patch":{"status":"running","total_paused_ms":1200},"uuid":"u-tu-2",'"$S"'}'
  # Frames for an id no task_started announced: never attached anywhere.
  emit '{"type":"system","subtype":"task_progress","task_id":"ghost","description":"ghost","usage":{"total_tokens":1,"tool_uses":0,"duration_ms":1},"uuid":"u-g-1",'"$S"'}'
  emit '{"type":"system","subtype":"task_updated","task_id":"ghost","patch":{"status":"completed"},"uuid":"u-g-2",'"$S"'}'
  emit '{"type":"system","subtype":"task_notification","task_id":"ghost","tool_use_id":"toolu_ghost","status":"completed","output_file":"","summary":"ghost","uuid":"u-g-3",'"$S"'}'
  # Experimental teammate (only with external opt-in) and a CC-08 shell job.
  emit '{"type":"system","subtype":"task_started","task_id":"tm1","tool_use_id":"toolu_team","description":"reviewer teammate","task_type":"in_process_teammate","uuid":"u-ts-2",'"$S"'}'
  emit '{"type":"system","subtype":"task_started","task_id":"bg1","tool_use_id":"toolu_bash","description":"sleep 1","task_type":"local_bash","uuid":"u-ts-3",'"$S"'}'
  emit '{"type":"system","subtype":"task_notification","task_id":"bg1","tool_use_id":"toolu_bash","status":"completed","output_file":"","summary":"sleep 1","uuid":"u-tn-0",'"$S"'}'
  emit '{"type":"system","subtype":"task_notification","task_id":"tm1","tool_use_id":"toolu_team","status":"stopped","reason":"worker_restart","output_file":"","summary":"reviewer teammate","uuid":"u-tn-1",'"$S"'}'
  emit '{"type":"system","subtype":"background_tasks_changed","tasks":[],"uuid":"u-btc-2",'"$S"'}'
  emit '{"type":"system","subtype":"task_updated","task_id":"wf1","patch":{"status":"completed","end_time":1790000009000},"uuid":"u-tu-3",'"$S"'}'
  emit '{"type":"system","subtype":"task_notification","task_id":"wf1","tool_use_id":"toolu_wf","status":"completed","output_file":"/tmp/claude-501/-tmp/sess-wf/tasks/wf1.output","summary":"Dynamic workflow \"Audit routes\" completed","usage":{"total_tokens":3400,"tool_uses":6,"duration_ms":9500},"uuid":"u-tn-2",'"$S"'}'
  # Late frames after the terminal: no resurrection, no flip.
  emit '{"type":"system","subtype":"task_progress","task_id":"wf1","tool_use_id":"toolu_wf","description":"late","usage":{"total_tokens":9999,"tool_uses":9,"duration_ms":9999},"uuid":"u-tp-late",'"$S"'}'
  emit '{"type":"system","subtype":"task_notification","task_id":"wf1","tool_use_id":"toolu_wf","status":"failed","output_file":"","summary":"late failure","uuid":"u-tn-late",'"$S"'}'
  # Wake turn: same session re-init (deduped) and a second result.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Workflow","Agent","Bash"],"cwd":"/tmp","session_id":"sess-wf"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"The audit found one blocked file."}]}}'
  emit '{"type":"result","subtype":"success","result":"The audit found one blocked file.","errors":[],"usage":{"input_tokens":5,"output_tokens":7},'"$S"'}'
  ;;

*scenario:workflow-abandoned*)
  # The provider process exits while its workflow is still running (crash,
  # reaper, app quit): nothing it owned can still be running afterwards.
  S='"session_id":"sess-wf-gone"'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Workflow"],"cwd":"/tmp","session_id":"sess-wf-gone"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_wf2","name":"Workflow","input":{"scriptPath":"/tmp/wf.js"}}]}}'
  emit '{"type":"system","subtype":"task_started","task_id":"wf2","tool_use_id":"toolu_wf2","description":"Long migration","task_type":"local_workflow","workflow_name":"migrate","prompt":"","uuid":"u-ts-9",'"$S"'}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_wf2","content":"Workflow running in background.","is_error":false}]}}'
  emit '{"type":"result","subtype":"success","result":"started","errors":[],"usage":{"input_tokens":1,"output_tokens":1},'"$S"'}'
  emit '{"type":"system","subtype":"task_progress","task_id":"wf2","tool_use_id":"toolu_wf2","description":"Migrate: a.js","usage":{"total_tokens":10,"tool_uses":1,"duration_ms":100},"workflow_progress":[{"type":"workflow_phase","index":1,"title":"Migrate","kind":"pipeline"},{"type":"workflow_agent","index":1,"label":"a.js","phaseIndex":1,"state":"progress"}],"uuid":"u-tp-9",'"$S"'}'
  ;;

*scenario:env-scrub*)
  # A LUNA-launched child must not inherit a parent Claude session's
  # messaging/bridge identity.
  for name in CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN CLAUDE_CODE_BRIDGE_SESSION_ID CLAUDE_CODE_SESSION_ID CLAUDE_CODE_CHILD_SESSION CLAUDE_CODE_SESSION_ATTENDED CLAUDE_JOB_DIR; do
    eval "present=\${$name+x}"
    if [ -n "$present" ]; then
      emit '{"type":"result","subtype":"error_during_execution","errors":["inherited '"$name"'"],"usage":{"input_tokens":0,"output_tokens":0},"session_id":"sess-env"}'
      exit 0
    fi
  done
  [ "$LUNA_ENV_SENTINEL" = "kept" ] || { emit '{"type":"result","subtype":"error_during_execution","errors":["unrelated env was scrubbed"],"usage":{"input_tokens":0,"output_tokens":0},"session_id":"sess-env"}'; exit 0; }
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":[],"cwd":"/tmp","session_id":"sess-env"}'
  emit '{"type":"result","subtype":"success","result":"clean","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-env"}'
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

*scenario:native-nested*)
  # CC-08 / E25: 2.1.286-shaped native agent graph (see the fixture). A run
  # without the subagent forwarding opt-in is a different CLI contract.
  forwarded=false
  for arg in "$@"; do
    [ "$arg" = '--forward-subagent-text' ] && forwarded=true
  done
  [ "$forwarded" = true ] || exit 1
  # Replay; a `luna_fixture_pause` line is the fixture's own timing marker
  # (never sent): the real wake turn starts seconds after the eager result.
  while IFS= read -r frame; do
    case "$frame" in
      *'"type":"luna_fixture_pause"'*) sleep 1.5 ;;
      *) emit "$frame" ;;
    esac
  done < "$(dirname "$0")/claude/native-agents-2.1.286.jsonl"
  ;;

*scenario:native-stop*)
  # CC-08: stop one background native child through `stop_task` (2.1.286
  # control request shape). Only that request may reach stdin; the child then
  # settles from its own task frames, like the real CLI.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Agent"],"cwd":"/tmp","session_id":"sess-stop"}'
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"model":"claude-fable-5","content":[{"type":"tool_use","id":"toolu_S","name":"Agent","input":{"description":"Watch the build","prompt":"Watch CI until it finishes.","run_in_background":true}}]}}'
  emit '{"type":"system","subtype":"task_started","task_id":"agent-s","tool_use_id":"toolu_S","description":"Watch the build","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","session_id":"sess-stop"}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_S","content":"Async agent launched successfully. agentId: agent-s","is_error":false}]}}'
  emit '{"type":"result","subtype":"success","result":"Watcher launched.","errors":[],"usage":{"input_tokens":5,"output_tokens":5},"session_id":"sess-stop"}'
  emit '{"type":"assistant","parent_tool_use_id":"toolu_S","message":{"model":"claude-haiku-4-5-20251001","content":[{"type":"text","text":"Watching the build."}]},"session_id":"sess-stop"}'
  read -r req || exit 1
  case "$req" in
    *'"subtype":"stop_task"'*'"task_id":"agent-s"'*) ;;
    *) exit 1 ;;
  esac
  rid=$(printf '%s\n' "$req" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
  emit "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$rid\",\"response\":{}}}"
  emit '{"type":"system","subtype":"task_updated","task_id":"agent-s","patch":{"status":"killed","end_time":1790000000000},"session_id":"sess-stop"}'
  emit '{"type":"system","subtype":"task_notification","task_id":"agent-s","tool_use_id":"toolu_S","status":"stopped","output_file":"","summary":"Watch the build","session_id":"sess-stop"}'
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Agent"],"cwd":"/tmp","session_id":"sess-stop"}'
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Watcher stopped."}}}'
  emit '{"type":"result","subtype":"success","result":"Watcher stopped.","errors":[],"usage":{"input_tokens":3,"output_tokens":3},"session_id":"sess-stop"}'
  # Anything else written before stdin closes is a refused stop that leaked.
  if read -r extra; then
    emit '{"type":"result","subtype":"error_during_execution","errors":["unexpected stdin after stop"],"usage":{"input_tokens":0,"output_tokens":0},"session_id":"sess-stop"}'
  fi
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

*scenario:e24-resume*)
  # E24/B1: a resumed session reports a leftover background task first,
  # then a peer turn, and only then runs the queued human prompt.
  u1=$(uuid_of "$first")
  [ -n "$u1" ] || exit 1
  case "$first" in *'"content":"scenario:e24-resume"'*) ;; *) exit 1 ;; esac
  init286 sess-e24
  bg_notification sess-e24 1
  plain_text sess-e24 FOREIGN-WAKE se-f1
  wake_result sess-e24 res-foreign-origin
  # A peer turn whose vouched uuid LUNA never sent (B10).
  emit '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"PEER-TURN"}},"user_message_uuid":"11111111-1111-4111-8111-111111111111","uuid":"se-p1","session_id":"sess-e24"}'
  emit '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"PEER-TURN","errors":[],"usage":{"input_tokens":2,"output_tokens":2},"session_id":"sess-e24","user_message_uuid":"11111111-1111-4111-8111-111111111111","user_message_uuids":["11111111-1111-4111-8111-111111111111"],"origin":{"kind":"peer","from":"uds:/tmp/peer.sock"},"uuid":"res-foreign-echo"}'
  # The human prompt finally runs: stamped stream start + stamped assistant.
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"message_start\",\"message\":{}},\"user_message_uuid\":\"$u1\",\"user_message_uuids\":[\"$u1\"],\"uuid\":\"se-h0\",\"session_id\":\"sess-e24\"}"
  plain_text sess-e24 HUMAN-ANSWER se-h1
  emit "{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"message\":{\"model\":\"claude-fable-5\",\"content\":[{\"type\":\"text\",\"text\":\"HUMAN-ANSWER\"}]},\"user_message_uuid\":\"$u1\",\"uuid\":\"as-h1\",\"session_id\":\"sess-e24\"}"
  own_result sess-e24 "$u1" res-human
  # B7: a replayed copy and a historical result change nothing.
  own_result sess-e24 "$u1" res-human
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1,\"result\":\"old\",\"errors\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"session_id\":\"sess-e24\",\"historical\":true,\"uuid\":\"res-historical\"}"
  ;;

*scenario:e24-followup*)
  # E24/B2: a warm follow-up waits in the CLI queue behind a wake turn.
  u1=$(uuid_of "$first")
  init286 sess-fu
  stamped_text sess-fu "$u1" FIRST-ANSWER se-1
  own_result sess-fu "$u1" res-1
  read -r second || exit 1
  u2=$(uuid_of "$second")
  [ -n "$u2" ] && [ "$u2" != "$u1" ] || exit 1
  bg_notification sess-fu 2
  plain_text sess-fu WAKE-TURN se-w
  wake_result sess-fu res-2
  stamped_text sess-fu "$u2" SECOND-ANSWER se-2
  own_result sess-fu "$u2" res-3
  ;;

*scenario:e24-legacy*)
  # E24/B4: an older runtime (no version, no echo) keeps legacy completion.
  emit '{"type":"system","subtype":"init","model":"claude-fable-5","tools":["Bash"],"cwd":"/tmp","session_id":"sess-old","claude_code_version":"2.1.228"}'
  plain_text sess-old FIRST-ANSWER se-1
  emit '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-old","uuid":"res-1"}'
  read -r second || exit 1
  bg_notification sess-old 2
  plain_text sess-old WAKE-TURN se-w
  wake_result sess-old res-2
  plain_text sess-old SECOND-ANSWER se-2
  emit '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-old","uuid":"res-3"}'
  ;;

*scenario:e24-fold*)
  # E24/B6: the CLI folds the queued follow-up into its own wake turn.
  u1=$(uuid_of "$first")
  init286 sess-fold
  stamped_text sess-fold "$u1" FIRST-ANSWER se-1
  own_result sess-fold "$u1" res-1
  read -r second || exit 1
  u2=$(uuid_of "$second")
  bg_notification sess-fold 2
  plain_text sess-fold WAKE-PART se-w
  stamped_text sess-fold "$u2" FOLDED-ANSWER se-2
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":2,\"result\":\"folded\",\"errors\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":4},\"session_id\":\"sess-fold\",\"origin\":{\"kind\":\"task-notification\"},\"user_message_uuid\":\"$u2\",\"user_message_uuids\":[\"$u2\"],\"uuid\":\"res-2\"}"
  ;;

*scenario:e24-batch*)
  # E24/B5: two follow-ups merged into one turn.
  u1=$(uuid_of "$first")
  init286 sess-batch
  stamped_text sess-batch "$u1" FIRST-ANSWER se-1
  own_result sess-batch "$u1" res-1
  read -r second || exit 1
  read -r third || exit 1
  u2=$(uuid_of "$second")
  u3=$(uuid_of "$third")
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"BATCH-ANSWER\"}},\"user_message_uuid\":\"$u3\",\"user_message_uuids\":[\"$u2\",\"$u3\"],\"uuid\":\"se-2\",\"session_id\":\"sess-batch\"}"
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1,\"result\":\"batch\",\"errors\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":4},\"session_id\":\"sess-batch\",\"user_message_uuid\":\"$u3\",\"user_message_uuids\":[\"$u2\",\"$u3\"],\"uuid\":\"res-2\"}"
  ;;

*scenario:e24-engine*)
  # Engine E2E: leftover foreign turn first; the human turn waits for the
  # test's release file in the run cwd; then one queued follow-up.
  u1=$(uuid_of "$first")
  init286 sess-eng
  bg_notification sess-eng 1
  plain_text sess-eng FOREIGN-LEFTOVER se-f1
  wake_result sess-eng res-foreign
  n=0
  while [ ! -f ./e24-release ]; do
    sleep 0.05
    n=$((n+1))
    [ "$n" -lt 400 ] || exit 1
  done
  stamped_text sess-eng "$u1" HUMAN-ANSWER se-h1
  own_result sess-eng "$u1" res-human
  read -r second || exit 0
  u2=$(uuid_of "$second")
  stamped_text sess-eng "$u2" SECOND-ANSWER se-h2
  own_result sess-eng "$u2" res-second
  # Post-turn lifecycle while the session is parked (journal-only evidence).
  bg_notification sess-eng 3
  # Stay warm like the real CLI until LUNA closes stdin.
  while read -r _; do :; done
  ;;

*scenario:cc02-lifecycle*)
  init286 sess-life
  emit '{"type":"system","subtype":"thinking_tokens","estimated_tokens":5,"uuid":"tt-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"thinking_tokens","estimated_tokens":9,"uuid":"tt-2","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"session_state_changed","state":"running","uuid":"ss-1","session_id":"sess-life"}'
  # Agent spawn + task lifecycle.
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_spawn","name":"Agent","input":{"description":"probe","prompt":"look around","run_in_background":true}}]},"uuid":"as-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"task_started","task_id":"agent-1","tool_use_id":"toolu_spawn","description":"probe","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","prompt":"look around","uuid":"ts-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"task_progress","task_id":"agent-1","tool_use_id":"toolu_spawn","description":"probe","usage":{"total_tokens":120,"tool_uses":2,"duration_ms":900},"last_tool_name":"Read","uuid":"tp-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"task_updated","task_id":"agent-1","patch":{"status":"killed","error":"stopped by user","end_time":5},"uuid":"tu-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"agent-1","task_type":"local_agent","description":"probe"}],"uuid":"bt-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"task_notification","task_id":"agent-1","tool_use_id":"toolu_spawn","status":"stopped","output_file":"/tmp/agent-1.out","summary":"stopped","usage":{"total_tokens":130,"tool_uses":2,"duration_ms":950},"uuid":"tn-1","session_id":"sess-life"}'
  # Hooks (one oversized stdout).
  emit '{"type":"system","subtype":"hook_started","hook_id":"h-1","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","uuid":"hs-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"hook_progress","hook_id":"h-1","hook_name":"PreToolUse:Bash","hook_event":"PreToolUse","stdout":"checking","stderr":"","output":"checking","uuid":"hp-1","session_id":"sess-life"}'
  big=$(head -c 6000 /dev/zero | tr '\0' 'x')
  emit "{\"type\":\"system\",\"subtype\":\"hook_response\",\"hook_id\":\"h-1\",\"hook_name\":\"PreToolUse:Bash\",\"hook_event\":\"PreToolUse\",\"output\":\"ok\",\"stdout\":\"$big\",\"stderr\":\"\",\"exit_code\":0,\"outcome\":\"success\",\"uuid\":\"hr-1\",\"session_id\":\"sess-life\"}"
  # Compaction (failed result) and reset.
  emit '{"type":"system","subtype":"status","status":"compacting","uuid":"st-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":959489,"post_tokens":10107,"duration_ms":3000},"logical_parent_uuid":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","uuid":"cb-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"summary too long","uuid":"st-2","session_id":"sess-life"}'
  emit '{"type":"conversation_reset","new_conversation_id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","trigger":"clear","uuid":"cr-1","session_id":"sess-life"}'
  emit '{"type":"command_lifecycle","command_uuid":"cccccccc-cccc-4ccc-8ccc-cccccccccccc","state":"queued","uuid":"cl-1","session_id":"sess-life"}'
  # Documented-but-unprojected, unknown (with secrets), oversized, flood,
  # non-JSON and invalid frames.
  emit '{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_x","message":"denied","uuid":"pd-1","session_id":"sess-life"}'
  emit '{"type":"prompt_suggestion","suggestion":"try again","uuid":"ps-1","session_id":"sess-life"}'
  emit '{"type":"system","subtype":"future_widget","api_key":"SECRET-SENTINEL-1","authorization":{"bearer":"SECRET-SENTINEL-2"},"input_tokens":5,"note":"kept","uuid":"fw-1","session_id":"sess-life"}'
  long=$(head -c 20000 /dev/zero | tr '\0' 'y')
  emit "{\"type\":\"future_frame\",\"nested\":{\"a\":{\"b\":{\"c\":{\"d\":{\"e\":{\"f\":{\"g\":{\"h\":\"deep\"}}}}}}}},\"blob\":\"$long\",\"uuid\":\"ff-1\"}"
  i=0
  while [ "$i" -lt 12 ]; do
    emit "{\"type\":\"future_flood\",\"n\":$i}"
    i=$((i+1))
  done
  emit 'this is not json'
  emit '{"type":"assistant","message":"not-an-object"}'
  # Tool-result bodies: string, rich array, empty, absent, subagent.
  emit '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t-str","name":"Bash","input":{"command":"echo hi"}},{"type":"tool_use","id":"t-arr","name":"mcp__demo__fetch","input":{}},{"type":"tool_use","id":"t-empty","name":"Bash","input":{"command":"true"}},{"type":"tool_use","id":"t-none","name":"Bash","input":{"command":"true"}}]},"uuid":"as-2","session_id":"sess-life"}'
  emit '{"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t-str","content":"hi\n","is_error":false},{"type":"tool_result","tool_use_id":"t-arr","content":[{"type":"text","text":"line one"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"QUJDREVG"}},{"type":"widget_v9","x":1}],"is_error":false},{"type":"tool_result","tool_use_id":"t-empty","content":"","is_error":false},{"type":"tool_result","tool_use_id":"t-none","is_error":false}]},"tool_use_result":{"stdout":"hi\n","stderr":"","interrupted":false},"uuid":"us-2","session_id":"sess-life"}'
  emit '{"type":"user","parent_tool_use_id":"toolu_spawn","message":{"content":[{"type":"tool_result","tool_use_id":"sub-t","content":"(Bash completed with no output)","is_error":false}]},"uuid":"us-3","session_id":"sess-life"}'
  # Tolerant result: malformed errors/usage; success + is_error (529).
  emit '{"type":"result","subtype":"success","is_error":true,"api_error_status":529,"result":"API Error: 529 overloaded","errors":{"unexpected":"object"},"usage":"not-an-object","num_turns":2,"total_cost_usd":0.0123,"stop_reason":"end_turn","terminal_reason":"completed","permission_denials":[{"tool_name":"Bash"}],"local_command":"compact","session_id":"sess-life","uuid":"res-life"}'
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
