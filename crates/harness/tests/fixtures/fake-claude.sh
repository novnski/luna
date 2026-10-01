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

case "$first" in

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
