#!/bin/sh
# Multi-turn fake Claude Code CLI for cross-session ("peer") and LUNA child
# chat scenarios. Every stdin user line is one turn, and the process stays
# warm until stdin EOF, like the real `-p --input-format stream-json` CLI.
#
# Frame shapes are copied from CLI 2.1.286 captures in
# luna-feature-checkouts/2026-10-01-goal-child-ui/evidence/cc09-children/probes/ingress
# (replay echo + origin, host-injected peer origin, stdin peer_message_hold
# held/dropped, system/init tool list). The socket-lane arrival and hold use the
# same fields as the binary's SDK schema (`origin.verifiedPeerPid`, lane
# "socket"); a live socket capture needs a second authenticated session.
#
# Each invocation logs `ARGV <pid> <args>` and `LINE <pid> <stdin line>` to
# <physical cwd>/.cc09-fake.log, so tests can assert resume ids and stdin origins.
# Session ids come from `--resume=` or from a `sid:NAME` token in the first line.

# Like the real CLI, report the physical cwd (LUNA validates it on binding).
cwd=$(pwd -P)
log="$cwd/.cc09-fake.log"
# One physical line per invocation (the appended system prompt has newlines).
printf 'ARGV %s %s\n' "$$" "$(printf '%s ' "$@" | tr '\n\r' '  ')" >> "$log"

resume=""
replay=false
for arg in "$@"; do
  case "$arg" in
    --resume=*) resume="${arg#--resume=}" ;;
    --replay-user-messages) replay=true ;;
  esac
done

emit() { printf '%s\n' "$1"; }
sid="$resume"
turn=0

result() {
  emit "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"$1\",\"errors\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"session_id\":\"$sid\",\"total_cost_usd\":0}"
}
error_result() {
  emit "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"errors\":[\"$1\"],\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"session_id\":\"$sid\"}"
}
init() {
  emit "{\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"$cwd\",\"session_id\":\"$sid\",\"tools\":[\"Task\",\"Read\",\"ListAgents\",\"SendMessage\"],\"model\":\"claude-haiku-4-5-20251001\",\"permissionMode\":\"acceptEdits\"}"
}
say() {
  emit "{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"session_id\":\"$sid\",\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"$1\"}}}"
  emit "{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"session_id\":\"$sid\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-haiku-4-5-20251001\",\"content\":[{\"type\":\"text\",\"text\":\"$1\"}]}}"
}
plain_echo() {
  emit "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"(replayed stdin turn)\"},\"session_id\":\"$sid\",\"parent_tool_use_id\":null,\"uuid\":\"echo-$$-$turn\",\"timestamp\":\"2026-10-01T14:23:27.494Z\",\"isReplay\":true}"
}
socket_arrival() {
  emit "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Another Claude session sent a message:\\n<cross-session-message from=\\\"uds:/tmp/cc09-peer.sock\\\" from-name=\\\"Terminal session\\\">\\nplease run the tests\\n</cross-session-message>\"},\"session_id\":\"$sid\",\"parent_tool_use_id\":null,\"uuid\":\"$1\",\"timestamp\":\"2026-10-01T14:30:00.000Z\",\"isReplay\":true,\"isSynthetic\":true,\"origin\":{\"kind\":\"peer\",\"from\":\"uds:/tmp/cc09-peer.sock\",\"name\":\"Terminal session\",\"verifiedPeerPid\":4242,\"body\":\"please run the tests\"}}"
}

if [ "$replay" != true ]; then
  error_result "--replay-user-messages missing"
  exit 3
fi

while IFS= read -r line; do
  case "$line" in
    *'"type":"control_request"'*) continue ;;
    *'"type":"control_response"'*) continue ;;
  esac
  printf 'LINE %s %s\n' "$$" "$line" >> "$log"
  turn=$((turn + 1))
  if [ -z "$sid" ]; then
    name=$(printf '%s' "$line" | sed -n 's/.*sid:\([a-z0-9-]*\).*/\1/p')
    sid="sess-${name:-$$}"
  fi
  case "$line" in
  *'expect-resume:'*)
    want=$(printf '%s' "$line" | sed -n 's/.*expect-resume:\([a-z0-9-]*\).*/\1/p')
    if [ "$resume" != "sess-$want" ]; then
      error_result "expected --resume=sess-$want, got '$resume'"
      continue
    fi
    ;;
  esac
  case "$line" in
  *'<cross-session-message from=\"luna-child:'*)
    # LUNA's own child report: must declare a peer origin with a sender class
    # and must never be stamped human.
    case "$line" in
      *'"kind":"human"'*) error_result "child report stamped human"; continue ;;
    esac
    case "$line" in
      *'"origin":{'*'"kind":"peer"'*) ;;
      *) error_result "child report lacks peer origin"; continue ;;
    esac
    case "$line" in
      *'"fromMode":"prompting"'*) mode=prompting ;;
      *'"fromMode":"bypass"'*) mode=bypass ;;
      *) error_result "child report lacks fromMode"; continue ;;
    esac
    child=$(printf '%s' "$line" | sed -n 's/.*from=\\"luna-child:\([A-Za-z0-9_-]*\)\\".*/\1/p')
    case "$line" in
    *'from-name=\"Held '*)
      # Receive policy parks it (as for a managed crossSessionInbound: hold).
      emit "{\"type\":\"system\",\"subtype\":\"peer_message_hold\",\"state\":\"held\",\"lane\":\"stdin\",\"from\":\"luna-child:$child\",\"from_name\":\"Held child\",\"cause\":\"managed-setting\",\"uuid\":\"hold-$$-$turn\",\"session_id\":\"$sid\"}"
      continue
      ;;
    esac
    emit "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Another Claude session sent a message:\\n(envelope)\"},\"session_id\":\"$sid\",\"parent_tool_use_id\":null,\"uuid\":\"echo-$$-$turn\",\"isReplay\":true,\"isSynthetic\":true,\"origin\":{\"kind\":\"peer\",\"from\":\"luna-child:$child\",\"fromSession\":\"$child\",\"fromMode\":\"$mode\",\"body\":\"(report)\",\"hostInjected\":true}}"
    init
    say "PARENT_SAW_REPORT"
    result "PARENT_SAW_REPORT"
    ;;
  *'scenario:peer-arrival'*)
    plain_echo; init; say "ACK"; result "ACK"
    # Idle session, then another session's SendMessage arrives on the socket.
    sleep 2
    socket_arrival "peer-msg-1"
    init; say "PEER_REPLY"; result "PEER_REPLY"
    ;;
  *'scenario:mid-turn-peer'*)
    plain_echo; init; say "BEFORE_PEER"
    socket_arrival "peer-msg-3"
    say "AFTER_PEER"; result "AFTER_PEER"
    ;;
  *'scenario:peer-hold-socket'*)
    plain_echo; init; say "HOLD_ACK"; result "HOLD_ACK"
    emit "{\"type\":\"system\",\"subtype\":\"peer_message_hold\",\"state\":\"held\",\"message_uuid\":\"peer-msg-2\",\"lane\":\"socket\",\"from\":\"uds:/tmp/cc09-peer.sock\",\"from_name\":\"Bypass session\",\"cause\":\"mode-mismatch\",\"uuid\":\"hold-$$-a\",\"session_id\":\"$sid\"}"
    emit "{\"type\":\"system\",\"subtype\":\"peer_message_hold\",\"state\":\"dropped\",\"message_uuid\":\"peer-msg-2\",\"lane\":\"socket\",\"from\":\"uds:/tmp/cc09-peer.sock\",\"from_name\":\"Bypass session\",\"outcome\":\"expired\",\"uuid\":\"hold-$$-b\",\"session_id\":\"$sid\"}"
    ;;
  *'sid:child'*|*'sid:held'*|*'sid:parity'*)
    plain_echo; init; say "CHILD_RESULT_OK"; result "CHILD_RESULT_OK"
    ;;
  *'child reply'*)
    plain_echo; init; say "CHILD_REPLY_OK"; result "CHILD_REPLY_OK"
    ;;
  *)
    plain_echo; init; say "OK"; result "OK"
    ;;
  esac
done
exit 0
