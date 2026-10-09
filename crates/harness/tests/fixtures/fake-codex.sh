#!/bin/sh
# Fake Codex app-server for luna-harness tests.
#
# Speaks scripted JSON-RPC 2.0 over stdio: initialize handshake, thread
# start/resume, then a scenario picked from the turn/start prompt text. Driven
# by crates/harness/tests/codex.rs.

emit() { printf '%s\n' "$1"; }
cid() { printf '%s' "$1" | sed 's/.*"clientUserMessageId":"\([^"]*\)".*/\1/'; }
rid() { printf '%s' "$1" | sed 's/.*"id":\([0-9]*\).*/\1/'; }
has() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }
[ -z "$CODEX_HOME" ] || mkdir -p "$CODEX_HOME"

# ---- LUNA MCP client (what Codex does with `-c mcp_servers.luna.*`) ---------
mcp_url=''
for arg in "$@"; do
  case "$arg" in
    mcp_servers.luna.url=*) mcp_url=${arg#mcp_servers.luna.url=}; mcp_url=${mcp_url#\"}; mcp_url=${mcp_url%\"} ;;
  esac
done
argv="$*"
mcp() { # $1 = JSON-RPC body; prints the HTTP response body
  curl -sS -m 30 -X POST "$mcp_url" -H "Authorization: Bearer $LUNA_MCP_BEARER_TOKEN" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' --data "$1"
}
mcp_tool() { # $1 id, $2 tool, $3 arguments JSON, $4 thread, $5 turn, $6 call id
  mcp "{\"jsonrpc\":\"2.0\",\"id\":$1,\"method\":\"tools/call\",\"params\":{\"name\":\"$2\",\"arguments\":$3,\"_meta\":{\"threadId\":\"$4\",\"callId\":\"$6\",\"x-codex-turn-metadata\":{\"turn_id\":\"$5\"}}}}"
}

fail_turn() { # $1 = request id, $2 = message
  emit "{\"id\":$1,\"result\":{\"turn\":{\"id\":\"t-bad\"}}}"
  # v2 has no turn/failed: a failed turn is turn/completed status=failed.
  emit "{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"th-1\",\"turn\":{\"id\":\"t-bad\",\"status\":\"failed\",\"error\":{\"message\":\"$2\"}}}}"
}

# Real `thread/turns/list` page shape (probe frames-turns-list.jsonl).
turn_page() { # $1 = request line, $2 = data json
  emit "{\"id\":$(rid "$1"),\"result\":{\"data\":$2,\"nextCursor\":null,\"backwardsCursor\":null}}"
}

# ---- handshake -------------------------------------------------------------
read -r line || exit 1 # initialize
has "$line" '"method":"initialize"' || exit 1
has "$line" '"experimentalApi":true' || exit 1
if ! has "$line" '"name":"luna-native"' && ! has "$line" '"name":"luna-reconciler"'; then
  exit 1
fi
emit "{\"id\":$(rid "$line"),\"result\":{\"userAgent\":\"fake-codex\"}}"

read -r line || exit 1 # initialized notification (no reply)
has "$line" '"method":"initialized"' || exit 1

# ---- thread start / resume -------------------------------------------------
read -r line || exit 1
# A cold "send as goal" clears the previous goal BEFORE loading the thread, so
# thread/resume cannot auto-start the stale goal's continuation (real 0.159,
# evidence/goal-parity/probes/p4-cold-metadata). Codex always notifies.
pre_cleared=''
if has "$line" '"method":"thread/goal/clear"'; then
  pre_cleared=$(printf '%s' "$line" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')
  emit "{\"method\":\"thread/goal/cleared\",\"params\":{\"threadId\":\"$pre_cleared\"}}"
  emit "{\"id\":$(rid "$line"),\"result\":{\"cleared\":true}}"
  read -r line || exit 1
fi
# Fork history reads page `thread/turns/list` (thread/read includeTurns is
# deprecated). Turn objects follow the probe's real page shape.
if has "$line" '"method":"thread/turns/list"'; then
  thread_id=$(printf '%s' "$line" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')
  has "$line" '"sortDirection":"asc"' || exit 1
  has "$line" '"itemsView":"full"' || exit 1
  case "$thread_id" in
    fork-source-preflight-fail)
      emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32603,\"message\":\"injected fork preflight failure\"}}"
      exit 0
      ;;
    fork-source-good|fork-source-mismatch|fork-source-timeout)
      turn_page "$line" '[{"id":"source-turn-1","status":"completed","itemsView":"full","error":null,"items":[{"id":"source-item-1","type":"agentMessage","text":"one","payload":{"id":"semantic-id","time":17}}]}]'
      exec sleep 30
      ;;
    forked-fork-source-good|forked-fork-source-mismatch)
      text=one
      if [ "$thread_id" = 'forked-fork-source-mismatch' ]; then text=changed; fi
      turn_page "$line" "[{\"id\":\"fork-turn-1\",\"status\":\"completed\",\"itemsView\":\"full\",\"error\":null,\"items\":[{\"id\":\"fork-item-1\",\"type\":\"agentMessage\",\"text\":\"$text\",\"payload\":{\"id\":\"semantic-id\",\"time\":17}}]}]"
      exec sleep 30
      ;;
  esac
  turn_page "$line" '[]'
  exec sleep 30
fi
if has "$line" '"method":"thread/read"'; then
  # Full-history hydration is deprecated; LUNA reads metadata only here.
  if has "$line" '"includeTurns":true'; then exit 1; fi
  thread_id=$(printf '%s' "$line" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')
  case "$thread_id" in
    fork-source-preflight-fail)
      emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32603,\"message\":\"injected fork preflight failure\"}}"
      exit 0
      ;;
    forked-fork-source-timeout)
      exec sleep 30
      ;;
    forked-fork-source-good|forked-fork-source-mismatch)
      emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"$thread_id\",\"name\":\"Forked\",\"cwd\":\"/tmp\",\"status\":{\"type\":\"idle\"},\"updatedAt\":1770000000,\"cliVersion\":\"0.154.0\",\"projectId\":null,\"ephemeral\":false}}}"
      exec sleep 30
      ;;
  esac
  if has "$line" '"threadId":"missing-thread"'; then
    emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32602,\"message\":\"thread not found\"}}"
    exit 0
  fi
  status=idle
  name='[LUNA QA] Imported fixture'
  if [ "$thread_id" = 'qa-active' ]; then status=active; fi
  if [ "$thread_id" = 'qa-plain' ]; then name='Foo'; fi
  if [ "$thread_id" = 'qa-recovery' ]; then name='Recovery fixture'; fi
  if [ "$thread_id" = 'qa-archived' ]; then name='Archived fixture'; fi
  case "$thread_id" in
    qa-plain|qa-recovery|qa-archived)
      if [ -f "$CODEX_HOME/fake-name-$thread_id" ]; then
        name=$(sed -n '1p' "$CODEX_HOME/fake-name-$thread_id")
      fi
      ;;
  esac
  emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"$thread_id\",\"name\":\"$name\",\"cwd\":\"/tmp\",\"status\":{\"type\":\"$status\"},\"updatedAt\":1770000000,\"cliVersion\":\"0.154.0\",\"projectId\":null,\"ephemeral\":false}}}"
  exec sleep 30
fi
if has "$line" '"method":"thread/fork"'; then
  # The fork response must not hydrate deprecated full history.
  has "$line" '"excludeTurns":true' || exit 1
  thread_id=$(printf '%s' "$line" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')
  printf '%s\n' "$thread_id" >> "$CODEX_HOME/fork-mutations"
  emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"forked-$thread_id\"}}}"
  exec sleep 30
fi
if has "$line" '"method":"thread/list"'; then
  if has "$line" '"archived":true' && has "$line" 'Archived fixture'; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"id\":\"qa-archived\"}],\"nextCursor\":null}}"
  elif has "$line" '"archived":false' && has "$line" 'Imported fixture'; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"id\":\"qa-import\"}],\"nextCursor\":null}}"
  elif has "$line" '"archived":false' && has "$line" 'Foo'; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"id\":\"qa-plain\"}],\"nextCursor\":null}}"
  elif has "$line" '"archived":false' && has "$line" 'Recovery fixture'; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"id\":\"qa-recovery\"}],\"nextCursor\":null}}"
  else
    emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[],\"nextCursor\":null}}"
  fi
  exec sleep 30
fi
if has "$line" '"method":"thread/name/set"'; then
  thread_id=$(printf '%s' "$line" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')
  name=$(printf '%s' "$line" | sed 's/.*"name":"\([^"]*\)".*/\1/')
  if [ "$thread_id" = 'qa-recovery' ] && [ ! -f "$CODEX_HOME/fake-rename-failed-$thread_id" ]; then
    : > "$CODEX_HOME/fake-rename-failed-$thread_id"
    emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32603,\"message\":\"injected one-shot rename failure\"}}"
    exit 0
  fi
  case "$thread_id" in
    qa-plain|qa-recovery|qa-archived)
      printf '%s\n' "$name" > "$CODEX_HOME/fake-name-$thread_id"
      ;;
  esac
  emit "{\"id\":$(rid "$line"),\"result\":{}}"
  exec sleep 30
fi
if has "$line" '"method":"thread/archive"' || has "$line" '"method":"thread/unarchive"'; then
  emit "{\"id\":$(rid "$line"),\"result\":{}}"
  exec sleep 30
fi
if has "$line" '"method":"config/read"'; then
  if [ -f "$CODEX_HOME/mcp-config-trace" ]; then
    pwd > "$CODEX_HOME/mcp-config-last-cwd"
    printf '%s\n' "$line" > "$CODEX_HOME/mcp-config-last-request"
  fi
  if [ -f "$CODEX_HOME/mcp-config-fail" ]; then
    emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32603,\"message\":\"synthetic MCP config failure\"}}"
    exec sleep 30
  fi
  if [ -f './.luna-mcp-project-a' ]; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"config\":{\"mcp_servers\":{\"project-a\":{\"enabled\":true}}}}}"
    exec sleep 30
  fi
  if [ -f './.luna-mcp-project-b' ]; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"config\":{\"mcp_servers\":{\"project-b\":{\"enabled\":true}}}}}"
    exec sleep 30
  fi
  emit "{\"id\":$(rid "$line"),\"result\":{\"config\":{\"mcp_servers\":{\"test\":{\"enabled\":true,\"command\":\"SECRET_COMMAND_SENTINEL\"},\"off\":{\"enabled\":false,\"env\":{\"TOKEN\":\"SECRET_TOKEN_SENTINEL\"}}}}}}"
  read -r line || exit 1
fi
thread_line="$line"
if has "$line" '"method":"skills/list"'; then
  if has "$line" '"cwds":'; then
    # Native L24 catalog read: the caller supplies an isolated home and cwd.
    if [ -f "$CODEX_HOME/skill-catalog-fail" ]; then
      emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32603,\"message\":\"synthetic catalog failure\"}}"
      exec sleep 30
    fi
    cwd=$(pwd)
    emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"cwd\":\"$cwd\",\"skills\":[{\"name\":\"imagegen\",\"path\":\"$cwd/.agents/skills/imagegen/SKILL.md\",\"enabled\":true},{\"name\":\"disabled\",\"path\":\"$cwd/.agents/skills/disabled/SKILL.md\",\"enabled\":false}]}]}}"
    exec sleep 30
  fi
  # Command discovery probe: answer with two cwd groups sharing one skill
  # (dedupe by name) and settle; no thread ever starts.
  emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"cwd\":\"/w\",\"skills\":[{\"name\":\"imagegen\",\"description\":\"Model-facing paragraph about images.\",\"interface\":{\"displayName\":\"Image Gen\",\"shortDescription\":\"Generate or edit images\"}},{\"name\":\"bare\",\"description\":\"No interface block\"}]},{\"cwd\":\"/x\",\"skills\":[{\"name\":\"imagegen\",\"description\":\"dupe\",\"interface\":{\"shortDescription\":\"dupe\"}}]}]}}"
  exec sleep 30
fi
if has "$line" '"method":"model/list"'; then
  # Live model discovery: force pagination and put the default model second,
  # proving the harness consumes nextCursor and honors isDefault.
  has "$line" '"includeHidden":false' || exit 1
  has "$line" '"limit":20' || exit 1
  emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"id\":\"gpt-5.6-terra\",\"model\":\"gpt-5.6-terra\",\"displayName\":\"GPT-5.6-Terra\",\"description\":\"Balanced agentic coding model for everyday work.\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"low\"},{\"reasoningEffort\":\"high\"}],\"additionalSpeedTiers\":[],\"serviceTiers\":[],\"defaultServiceTier\":null,\"isDefault\":false},{\"id\":\"gpt-6-astra\",\"model\":\"gpt-6-astra\",\"displayName\":\"GPT-6-Astra\",\"description\":\"Our most capable model for complex, demanding work.\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"low\"},{\"reasoningEffort\":\"medium\"},{\"reasoningEffort\":\"high\"},{\"reasoningEffort\":\"xhigh\"},{\"reasoningEffort\":\"max\"},{\"reasoningEffort\":\"ultra\"}],\"additionalSpeedTiers\":[\"fast\"],\"serviceTiers\":[{\"id\":\"priority\",\"name\":\"Fast\"}],\"defaultServiceTier\":null,\"isDefault\":true}],\"nextCursor\":\"page-2\"}}"
  read -r line || exit 1
  has "$line" '"method":"model/list"' || exit 1
  has "$line" '"cursor":"page-2"' || exit 1
  emit "{\"id\":$(rid "$line"),\"result\":{\"data\":[{\"id\":\"gpt-5.6-sol\",\"model\":\"gpt-5.6-sol\",\"displayName\":\"GPT-5.6-Sol\",\"description\":\"Reliable agentic workhorse for everyday tasks.\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"low\"},{\"reasoningEffort\":\"ultra\"}],\"additionalSpeedTiers\":[],\"serviceTiers\":[],\"defaultServiceTier\":null,\"isDefault\":false}],\"nextCursor\":null}}"
  exec sleep 30
fi
history=''
history_mode=''
if has "$line" '"method":"thread/resume"'; then
  # Cold resume is metadata-only: full-history hydration is deprecated
  # (thread_processor.rs:39); history is paged with thread/turns/list.
  has "$line" '"excludeTurns":true' || exit 1
  if has "$line" '"threadId":"compact-ok"'; then
    printf '%s\n' 'resume' >> "$CODEX_HOME/compact-trace"
    pwd > "$CODEX_HOME/compact-cwd"
    printf '%s\n' "$line" > "$CODEX_HOME/compact-resume-request"
    printf '%s\n' "$@" > "$CODEX_HOME/compact-argv"
    if [ -f "$CODEX_HOME/config.toml" ]; then
      cp "$CODEX_HOME/config.toml" "$CODEX_HOME/compact-config-observed"
    fi
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"compact-ok\",\"status\":{\"type\":\"idle\"}}}}"
    read -r compact_line || exit 1
    has "$compact_line" '"method":"thread/compact/start"' || exit 1
    has "$compact_line" '"threadId":"compact-ok"' || exit 1
    printf '%s\n' 'compact' >> "$CODEX_HOME/compact-trace"
    : > "$CODEX_HOME/compact-called"
    emit "{\"id\":$(rid "$compact_line"),\"result\":{}}"
    emit '{"method":"item/started","params":{"threadId":"compact-ok","turnId":"compact-turn","item":{"id":"compact-1","type":"contextCompaction"}}}'
    emit '{"method":"item/completed","params":{"threadId":"other-thread","turnId":"compact-turn","item":{"id":"compact-1","type":"contextCompaction"}}}'
    emit '{"method":"item/completed","params":{"threadId":"compact-ok","turnId":"compact-turn","item":{"id":"compact-stale","type":"contextCompaction"}}}'
    sleep 1
    printf '%s\n' 'matching-completion' >> "$CODEX_HOME/compact-trace"
    : > "$CODEX_HOME/compact-matching-completion"
    emit '{"method":"item/completed","params":{"threadId":"compact-ok","turnId":"compact-turn","item":{"id":"compact-1","type":"contextCompaction"}}}'
    exec sleep 30
  elif has "$line" '"threadId":"compact-load-fail"'; then
    printf '%s\n' 'resume-rejected' >> "$CODEX_HOME/compact-trace"
    emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32602,\"message\":\"bound thread could not be loaded\"}}"
    exit 0
  elif has "$line" '"threadId":"compact-owner-active"'; then
    printf '%s\n' 'resume-active' >> "$CODEX_HOME/compact-trace"
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"compact-owner-active\",\"status\":{\"type\":\"active\",\"activeFlags\":[\"turn\"]}}}}"
    exec sleep 30
  elif has "$line" '"threadId":"compact-wrong-binding"'; then
    printf '%s\n' 'resume-wrong-binding' >> "$CODEX_HOME/compact-trace"
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"different-thread\",\"status\":{\"type\":\"idle\"}}}}"
    exec sleep 30
  elif has "$line" '"threadId":"resume-with-child-v1"'; then
    # excludeTurns: no turns here; ownership comes from the history pages.
    history='[{"id":"old-turn","status":"completed","itemsView":"full","error":null,"items":[{"type":"collabAgentToolCall","id":"spawn-alpha","tool":"spawnAgent","status":"completed","senderThreadId":"th-resumed","receiverThreadIds":["child-alpha"],"prompt":null,"model":null,"reasoningEffort":null,"agentsStates":{}}]}]'
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-resumed\",\"historyMode\":\"paginated\",\"turns\":[]}}}"
  elif has "$line" '"threadId":"resume-with-child-legacy"'; then
    # A legacy (non-paginated) thread: thread/turns/list would replay the
    # rollout once per page, so the adapter must read the history once.
    history_mode=legacy
    history='[{"id":"old-turn","status":"completed","items":[{"type":"collabAgentToolCall","id":"spawn-alpha","tool":"spawnAgent","status":"completed","senderThreadId":"th-resumed","receiverThreadIds":["child-alpha"],"prompt":null,"model":null,"reasoningEffort":null,"agentsStates":{}}]}]'
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-resumed\",\"historyMode\":\"legacy\",\"turns\":[]}}}"
  elif has "$line" '"threadId":"resume-with-child-v2"'; then
    history='[{"id":"old-turn","status":"completed","itemsView":"full","error":null,"items":[{"type":"subAgentActivity","id":"spawn-alpha","kind":"started","agentThreadId":"child-alpha","agentPath":"/root/alpha"},{"type":"subAgentActivity","id":"subagent-completed-old","kind":"completed","agentThreadId":"child-alpha","agentPath":"/root/alpha"}]}]'
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-resumed\",\"historyMode\":\"paginated\",\"turns\":[]}}}"
  elif has "$line" '"threadId":"resume-fail"'; then
    # Missing/foreign rollout: reject, expect the fresh-start fallback.
    emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32600,\"message\":\"rollout not found\"}}"
    read -r line || exit 1
    has "$line" '"method":"thread/start"' || exit 1
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-fresh\"}}}"
  elif has "$line" '"threadId":"context-resume"'; then
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"context-resume\"}}}"
  elif has "$line" 'goal-warm-workspace'; then
    # The goal E2E's later sends resume the same native thread.
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"$(printf '%s' "$line" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')\"}}}"
  elif has "$line" '"threadId":"goal-resume"'; then
    if [ "$pre_cleared" != 'goal-resume' ]; then
      emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32600,\"message\":\"resume before the stale goal was cleared\"}}"
      exit 0
    fi
    has "$line" '"model_reasoning_summary":"auto"' || { emit "{\"id\":$(rid "$line"),\"error\":{\"code\":-32600,\"message\":\"goal resume without reasoning summaries\"}}"; exit 0; }
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"goal-resume\"}}}"
  else
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-resumed\"}}}"
  fi
elif has "$line" '"method":"thread/start"'; then
  if [ -f ./.luna-fake-thread-per-task ] && [ -n "$LUNA_THREAD_ID" ]; then
    # Several concurrent LUNA tasks need distinct provider threads.
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-$LUNA_THREAD_ID\"}}}"
  else
    emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"th-1\"}}}"
  fi
else
  exit 1
fi

# ---- first turn ------------------------------------------------------------
read -r turnline || exit 1
native_name_line=''
if has "$turnline" '"method":"thread/name/set"'; then
  native_name_line="$turnline"
  emit "{\"id\":$(rid "$turnline"),\"result\":{}}"
  read -r turnline || exit 1
fi
# A cold resume rebuilds sub-agent ownership from ascending full history.
if [ "$history_mode" = legacy ]; then
  has "$turnline" '"method":"thread/read"' && has "$turnline" '"includeTurns":true' || exit 1
  emit "{\"id\":$(rid "$turnline"),\"result\":{\"thread\":{\"id\":\"th-resumed\",\"historyMode\":\"legacy\",\"turns\":$history}}}"
  read -r turnline || exit 1
fi
while has "$turnline" '"method":"thread/turns/list"' && has "$turnline" '"sortDirection":"asc"'; do
  [ "$history_mode" = legacy ] && exit 1
  has "$turnline" '"itemsView":"full"' || exit 1
  turn_page "$turnline" "${history:-[]}"
  read -r turnline || exit 1
done
# Installed Codex 0.159 answers the adapter's startup goal readback
# (ThreadGoalGetResponse); these scenarios have no goal.
if has "$turnline" '"method":"thread/goal/get"'; then
  emit "{\"id\":$(rid "$turnline"),\"result\":{\"goal\":null}}"
  read -r turnline || exit 1
fi
# Resume steer reconciliation pages newest-first and stops once found.
if has "$turnline" '"method":"thread/turns/list"' && has "$turnline" '"threadId":"th-resumed"'; then
  has "$turnline" '"sortDirection":"desc"' || exit 1
  turn_page "$turnline" '[{"id":"prior-turn","status":"completed","itemsView":"full","error":null,"items":[{"type":"userMessage","id":"u-prior","clientId":"prior-steer","content":[]},{"type":"userMessage","id":"u-other","clientId":"unrelated-input","content":[]}]}]'
  read -r turnline || exit 1
fi
# ---- send as goal ----------------------------------------------------------
# Codex's own sequence on the loaded thread: clear, then set active. The server
# starts the goal turn itself (no turn/start, no userMessage item); the model's
# update_goal surfaces only as thread/goal/updated with turnId. Frames mirror
# evidence/goal-parity/probes/p1-start (installed 0.159.0, gpt-6.1-sol).
goal_json() { # $1 thread, $2 status, $3 time used, $4 tokens used
  printf '{"threadId":"%s","objective":"%s","status":"%s","tokenBudget":null,"tokensUsed":%s,"timeUsedSeconds":%s,"createdAt":1790858093,"updatedAt":1790858108}' "$1" "$goal_objective" "$2" "$4" "$3"
}
goal_turn() { # $1 thread, $2 turn id, $3 answer
  emit "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"$1\",\"turn\":{\"id\":\"$2\",\"items\":[],\"status\":\"inProgress\"}}}"
  emit "{\"method\":\"item/started\",\"params\":{\"threadId\":\"$1\",\"turnId\":\"$2\",\"item\":{\"type\":\"agentMessage\",\"id\":\"$2-msg\",\"text\":\"\"}}}"
  emit "{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"$1\",\"turnId\":\"$2\",\"itemId\":\"$2-msg\",\"delta\":\"$3\"}}"
  emit "{\"method\":\"thread/goal/updated\",\"params\":{\"threadId\":\"$1\",\"turnId\":\"$2\",\"goal\":$(goal_json "$1" active 14 21440)}}"
  emit "{\"method\":\"thread/goal/updated\",\"params\":{\"threadId\":\"$1\",\"turnId\":\"$2\",\"goal\":$(goal_json "$1" complete 14 21440)}}"
  emit "{\"method\":\"item/completed\",\"params\":{\"threadId\":\"$1\",\"turnId\":\"$2\",\"item\":{\"type\":\"agentMessage\",\"id\":\"$2-msg\",\"text\":\"$3\"}}}"
  emit "{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"$1\",\"turn\":{\"id\":\"$2\",\"items\":[],\"status\":\"completed\"}}}"
}
goal_start() { # $1 = the thread/goal/clear line; answers through the set
  gthread=$(printf '%s' "$1" | sed 's/.*"threadId":"\([^"]*\)".*/\1/')
  emit "{\"method\":\"thread/goal/cleared\",\"params\":{\"threadId\":\"$gthread\"}}"
  emit "{\"id\":$(rid "$1"),\"result\":{\"cleared\":false}}"
  read -r setline || exit 1
  if ! has "$setline" '"method":"thread/goal/set"' || ! has "$setline" '"status":"active"'; then
    emit "{\"method\":\"error\",\"params\":{\"threadId\":\"$gthread\",\"message\":\"expected thread/goal/set active after clear\"}}"
    exit 0
  fi
  goal_objective=$(printf '%s' "$setline" | sed 's/.*"objective":"\([^"]*\)".*/\1/')
  if has "$setline" 'scenario:goal-rejected'; then
    emit "{\"id\":$(rid "$setline"),\"error\":{\"code\":-32600,\"message\":\"goals feature is disabled\"}}"
    return 1
  fi
  emit "{\"id\":$(rid "$setline"),\"result\":{\"goal\":$(goal_json "$gthread" active 0 0)}}"
  emit "{\"method\":\"thread/goal/updated\",\"params\":{\"threadId\":\"$gthread\",\"turnId\":null,\"goal\":$(goal_json "$gthread" active 0 0)}}"
}
if has "$turnline" '"method":"thread/goal/clear"'; then
  goal_start "$turnline" || { exec sleep 2; }
  case "$setline" in
  *scenario:goal-no-turn*)
    # Codex accepted the goal but never started a turn (e.g. Plan mode).
    exec sleep 5
    ;;
  *)
    goal_turn "$gthread" goal-turn-1 '1\n2\n3'
    ;;
  esac
  # A turn/start here would be a second turn: the goal turn is Codex's own.
  # (`read -t` where the shell has it; otherwise skip the probe.)
  if read -t 1 -r extra 2>/dev/null && has "$extra" '"method":"turn/start"'; then
    emit "{\"method\":\"error\",\"params\":{\"threadId\":\"$gthread\",\"message\":\"unexpected turn/start after a goal start\"}}"
  fi
  exit 0
fi

tid=$(rid "$turnline")

case "$turnline" in
*scenario:resume-steering-reconciliation*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"new-turn\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-resumed","turn":{"id":"new-turn"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-resumed","turn":{"id":"new-turn","status":"completed"}}}'
  ;;

*scenario:mcp-form:*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-mcp\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-mcp"}}}'
  exec python3 "$(dirname "$0")/mcp-elicitation-peer.py" "$turnline"
  ;;
*scenario:computer-use:response-before-start*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t1"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t1","status":"completed"}}}'
  read -r nextturn || exit 1
  { has "$nextturn" '"method":"turn/start"' && has "$nextturn" 'follow-up t2'; } || exit 1
  emit "{\"id\":$(rid "$nextturn"),\"result\":{\"turn\":{\"id\":\"t2\"}}}"
  emit '{"id":922,"method":"mcpServer/elicitation/request","params":{"serverName":"computer-use","threadId":"th-1","turnId":"t2","mode":"form","message":"Allow ChatGPT to use Finder?","requestedSchema":{"properties":{"approval":{"oneOf":[{"const":"once"}]}}}}}'
  read -r answer || exit 1
  { has "$answer" '"action":"accept"' && has "$answer" '"approval":"once"'; } || { emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t2","status":"failed","error":{"message":"response-owned t2 approval was withheld"}}}}'; exit 0; }
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t2"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t2","status":"completed"}}}'
  ;;

*scenario:computer-use:late-t1-*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t1"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t1","status":"completed"}}}'
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t2"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t2","item":{"type":"mcpToolCall","id":"cu-t2","server":"computer-use","tool":"click","arguments":{"app":"Finder"},"status":"inProgress"}}}'
  emit '{"id":920,"method":"mcpServer/elicitation/request","params":{"serverName":"computer-use","threadId":"th-1","turnId":"t2","mode":"form","message":"Allow ChatGPT to use Finder?","requestedSchema":{"properties":{"approval":{"oneOf":[{"const":"once"}]}}}}}'
  if has "$turnline" 'late-t1-start'; then
    emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t1"}}}'
  elif has "$turnline" 'late-t1-completed'; then
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t1","status":"completed"}}}'
  elif has "$turnline" 'late-t1-failed'; then
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t1","status":"failed","error":{"message":"stale failure"}}}}'
  else
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t1","status":"interrupted"}}}'
  fi
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"stale-barrier","delta":"stale-delivered"}}'
  read -r answer || exit 1
  { has "$answer" '"action":"accept"' && has "$answer" '"approval":"once"'; } || { emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t2","status":"failed","error":{"message":"t2 approval was wrongly withheld"}}}}'; exit 0; }
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t2","item":{"type":"mcpToolCall","id":"cu-t2","server":"computer-use","tool":"click","status":"completed"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t2","status":"completed"}}}'
  ;;

*scenario:computer-use:late-answer*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t2\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t2"}}}'
  emit '{"id":921,"method":"mcpServer/elicitation/request","params":{"serverName":"computer-use","threadId":"th-1","turnId":"t2","mode":"form","message":"Allow ChatGPT to use Finder?","requestedSchema":{"properties":{"approval":{"oneOf":[{"const":"once"}]}}}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t2","status":"completed"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"end-barrier","delta":"turn-ended"}}'
  read -r answer || exit 1
  has "$answer" '"action":"decline"' || { emit '{"method":"error","params":{"threadId":"th-1","message":"late approval was accepted"}}'; exit 0; }
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"withheld","delta":"late-approval-withheld"}}'
  ;;

*scenario:computer-use:disconnect*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-computer\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-computer"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-computer","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","arguments":{"app":"Finder"},"status":"inProgress"}}}'
  exit 0
  ;;

*scenario:computer-use:cancel*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-computer\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-computer"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-computer","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","arguments":{"app":"Finder"},"status":"inProgress"}}}'
  read -r intline || exit 1
  if has "$intline" '"method":"turn/interrupt"' && has "$intline" '"turnId":"t-computer"'; then
    emit "{\"id\":$(rid "$intline"),\"result\":{}}"
    emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-computer","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","status":"completed"}}}'
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-computer","status":"interrupted"}}}'
  else
    fail_turn "$tid" "expected computer-use turn interrupt"
  fi
  ;;

*scenario:computer-use:permission-*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-computer\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-computer"}}}'
  emit '{"id":910,"method":"item/permissions/requestApproval","params":{"threadId":"th-1","turnId":"stale","itemId":"perm-1","permissions":{"network":{"enabled":true}},"reason":"Synthetic access"}}'
  read -r rejected || exit 1
  has "$rejected" '"permissions":{}' || { fail_turn "$tid" "stale permission was granted"; exit 0; }
  emit '{"id":911,"method":"item/permissions/requestApproval","params":{"threadId":"th-1","turnId":"t-computer","itemId":"perm-2","permissions":{"network":{"enabled":true}},"reason":"Synthetic access"}}'
  read -r answer || exit 1
  if has "$turnline" 'scenario:computer-use:permission-allow'; then
    has "$answer" '"permissions":{"network":{"enabled":true}}' || { fail_turn "$tid" "provider permission not granted once"; exit 0; }
  else
    has "$answer" '"permissions":{}' || { fail_turn "$tid" "provider permission not denied"; exit 0; }
  fi
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-computer","status":"completed"}}}'
  ;;

*scenario:computer-use:*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-computer\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-computer"}}}'
  emit '{"id":900,"method":"mcpServer/elicitation/request","params":{"serverName":"computer-use","threadId":"foreign","turnId":"t-computer","mode":"form","message":"Allow ChatGPT to use Finder?","requestedSchema":{"properties":{"approval":{"oneOf":[{"const":"once"},{"const":"always"}]}}}}}'
  read -r rejected || exit 1
  has "$rejected" '"action":"decline"' || { fail_turn "$tid" "foreign target was accepted"; exit 0; }
  emit '{"id":901,"method":"mcpServer/elicitation/request","params":{"serverName":"computer-use","threadId":"th-1","turnId":"t-computer","mode":"form","message":"Allow ChatGPT to use Finder?","requestedSchema":{"properties":{"approval":{"oneOf":[{"const":"once"},{"const":"always"}]}}}}}'
  read -r answer || exit 1
  if has "$turnline" 'scenario:computer-use:allow'; then
    { has "$answer" '"action":"accept"' && has "$answer" '"approval":"once"'; } || { emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-computer","status":"failed","error":{"message":"one-request approval was not returned"}}}}'; exit 0; }
    emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-computer","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","arguments":{"app":"Finder","x":10},"status":"inProgress"}}}'
    emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"stale-turn","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","status":"completed"}}}'
    emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-computer","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","status":"completed","result":{"content":[{"type":"image","data":"private pixels"}]}}}}'
    emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-computer","item":{"type":"mcpToolCall","id":"cu-1","server":"computer-use","tool":"click","status":"completed"}}}'
  else
    has "$answer" '"action":"decline"' || { fail_turn "$tid" "computer use was not denied"; exit 0; }
  fi
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-computer","status":"completed"}}}'
  ;;

*scenario:l25-send-storage-fail:*)
  target=$(printf '%s' "$turnline" | sed -n 's/.*scenario:l25-send-storage-fail:\([0-9a-f-]*\).*/\1/p')
  [ -n "$target" ] || { fail_turn "$tid" "missing storage-failure target"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-l25-storage-fail\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-l25-storage-fail"}}}'
  response=$(mcp_tool 89 send_message_to_thread "{\"taskId\":\"$target\",\"message\":\"Storage failure follow-up\",\"idempotencyKey\":\"l25-storage-fail\"}" th-1 t-l25-storage-fail call-storage-fail)
  has "$response" '"isError":true' || { fail_turn "$tid" "SQL failure returned queued success"; exit 0; }
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-l25-storage-fail","status":"completed"}}}'
  ;;

*scenario:l25-send-stale:*)
  target=$(printf '%s' "$turnline" | sed -n 's/.*scenario:l25-send-stale:\([0-9a-f-]*\).*/\1/p')
  [ -n "$target" ] || { fail_turn "$tid" "missing stale target"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-l25-stale\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-l25-stale"}}}'
  response=$(mcp_tool 86 send_message_to_thread "{\"taskId\":\"$target\",\"message\":\"Stale target follow-up\",\"idempotencyKey\":\"l25-stale-target\"}" th-1 t-l25-stale call-stale-target)
  has "$response" '"isError":true' || { fail_turn "$tid" "stale target accepted"; exit 0; }
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-l25-stale","status":"completed"}}}'
  ;;

*scenario:l5-automation:*)
  target=$(printf '%s' "$turnline" | sed -n 's/.*scenario:l5-automation:\([0-9a-f-]*\).*/\1/p')
  listed=$(mcp '{"jsonrpc":"2.0","id":180,"method":"tools/list","params":{}}')
  has "$listed" '"name":"automation_update"' || { fail_turn "$tid" "automation tool missing"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-automation\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-automation"}}}'
  args="{\"automationId\":\"agent-approved\",\"expectedRevision\":0,\"idempotencyKey\":\"agent-create\",\"operation\":{\"kind\":\"save\",\"definition\":{\"name\":\"Agent approved schedule\",\"prompt\":\"Inspect the release\",\"timezone\":\"UTC\",\"target\":{\"kind\":\"existing\",\"taskId\":\"$target\"},\"trigger\":{\"kind\":\"once\",\"atMs\":2051222400000},\"maxLatenessMs\":300000}}}"
  response=$(mcp_tool 181 automation_update "$args" foreign-thread t-automation call-wrong-task)
  has "$response" '"isError":true' || { fail_turn "$tid" "foreign automation source accepted"; exit 0; }
  response=$(mcp_tool 182 automation_update "$args" th-1 t-automation call-automation-create)
  has "$response" '"isError":false' && has "$response" 'agent-approved' || { fail_turn "$tid" "approved automation did not return receipt"; exit 0; }
  response=$(mcp_tool 183 automation_update "$args" th-1 t-automation call-automation-create)
  has "$response" '"isError":true' || { fail_turn "$tid" "replayed automation call accepted"; exit 0; }
  changed=$(printf '%s' "$args" | sed 's/Inspect the release/Different instructions/')
  response=$(mcp_tool 184 automation_update "$changed" th-1 t-automation call-automation-collision)
  has "$response" '"isError":true' || { fail_turn "$tid" "changed automation payload accepted"; exit 0; }
  denied=$(printf '%s' "$args" | sed 's/agent-approved/agent-denied/g; s/agent-create/agent-deny/g')
  response=$(mcp_tool 185 automation_update "$denied" th-1 t-automation call-automation-denied)
  has "$response" '"isError":true' || { fail_turn "$tid" "task-wide allowance created automation"; exit 0; }
  response=$(mcp_tool 186 list_automations '{}' th-1 t-automation call-automation-list)
  has "$response" '"isError":false' && has "$response" 'agent-approved' || { fail_turn "$tid" "automation list missing saved row"; exit 0; }
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-automation","status":"completed"}}}'
  ;;

*scenario:l25-send:*)
  target=$(printf '%s' "$turnline" | sed -n 's/.*scenario:l25-send:\([0-9a-f-]*\).*/\1/p')
  foreign=$(printf '%s' "$turnline" | sed -n 's/.*scenario:l25-send:[0-9a-f-]*:\([0-9a-f-]*\).*/\1/p')
  [ -n "$target" ] || { fail_turn "$tid" "missing synthetic L25 target"; exit 0; }
  listed=$(mcp '{"jsonrpc":"2.0","id":80,"method":"tools/list","params":{}}')
  has "$listed" '"name":"send_message_to_thread"' || { fail_turn "$tid" "L25 send tool missing"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-l25-send\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-l25-send"}}}'
  response=$(mcp_tool 81 send_message_to_thread "{\"taskId\":\"$target\",\"message\":\"foreign\",\"idempotencyKey\":\"l25-foreign\"}" foreign-thread t-l25-send call-wrong-task)
  has "$response" '"isError":true' || { fail_turn "$tid" "foreign source accepted"; exit 0; }
  if [ -n "$foreign" ]; then
    response=$(mcp_tool 85 send_message_to_thread "{\"taskId\":\"$foreign\",\"message\":\"wrong project\",\"idempotencyKey\":\"l25-wrong-target\"}" th-1 t-l25-send call-wrong-target)
    has "$response" '"isError":true' || { fail_turn "$tid" "wrong-project target accepted"; exit 0; }
  fi
  response=$(mcp_tool 82 send_message_to_thread "{\"taskId\":\"$target\",\"message\":\"Synthetic L25 queued follow-up\",\"idempotencyKey\":\"l25-send-1\"}" th-1 t-l25-send call-send-1)
  has "$response" '"isError":false' && has "$response" 'queued' || { fail_turn "$tid" "approved send did not return queued receipt"; exit 0; }
  response=$(mcp_tool 83 send_message_to_thread "{\"taskId\":\"$target\",\"message\":\"Synthetic L25 queued follow-up\",\"idempotencyKey\":\"l25-send-1\"}" th-1 t-l25-send call-send-1)
  has "$response" '"isError":true' || { fail_turn "$tid" "replayed provider call accepted"; exit 0; }
  response=$(mcp_tool 87 send_message_to_thread "{\"taskId\":\"$target\",\"message\":\"Different synthetic L25 follow-up\",\"idempotencyKey\":\"l25-send-1\"}" th-1 t-l25-send call-send-collision)
  has "$response" '"isError":true' || { fail_turn "$tid" "changed-payload command ID accepted"; exit 0; }
  response=$(mcp_tool 84 send_message_to_thread "{\"taskId\":\"$foreign\",\"message\":\"Denied synthetic L25 follow-up\",\"idempotencyKey\":\"l25-send-deny\"}" th-1 t-l25-send call-send-deny)
  has "$response" '"isError":true' || { fail_turn "$tid" "denied send accepted"; exit 0; }
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-l25-send","status":"completed"}}}'
  ;;

*scenario:mcp-hold*)
  # The engine E2E is the MCP client: publish this process's endpoint under
  # the run's cwd (the test's project dir, never CODEX_HOME), keep the turn
  # active until the test releases it, then finish the turn. Runs without a
  # LUNA endpoint (title generation) end at once.
  if [ -z "$mcp_url" ] || [ -z "$LUNA_THREAD_ID" ]; then fail_turn "$tid" "no LUNA MCP endpoint"; exit 0; fi
  thread=th-1
  if [ -f ./.luna-fake-thread-per-task ]; then thread="th-$LUNA_THREAD_ID"; fi
  if has "$thread_line" '"method":"thread/resume"'; then thread=th-resumed; fi
  turn="t-hold-$LUNA_THREAD_ID"
  hold_dir="$(pwd)/.luna-mcp-hold"
  mkdir -p "$hold_dir"
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"$turn\"}}}"
  emit "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"$thread\",\"turn\":{\"id\":\"$turn\"}}}"
  # The thread request (with LUNA developerInstructions) for identity checks.
  printf '%s\n' "$thread_line" > "$hold_dir/$LUNA_THREAD_ID.thread"
  printf '%s\n%s\n%s\n%s\n' "$mcp_url" "$LUNA_MCP_BEARER_TOKEN" "$thread" "$turn" > "$hold_dir/$LUNA_THREAD_ID.tmp"
  mv "$hold_dir/$LUNA_THREAD_ID.tmp" "$hold_dir/$LUNA_THREAD_ID"
  waited=0
  while [ ! -f "$hold_dir/release-$LUNA_THREAD_ID" ] && [ "$waited" -lt 1200 ]; do
    sleep 0.05; waited=$((waited + 1))
  done
  rm -f "$hold_dir/$LUNA_THREAD_ID" "$hold_dir/release-$LUNA_THREAD_ID"
  emit "{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"$thread\",\"turn\":{\"id\":\"$turn\",\"status\":\"completed\"}}}"
  ;;

*scenario:runtime-contract-resume*)
  # A resumed thread gets the per-process endpoint too: no dynamicTools are
  # (re)registered, yet the current tool set is listed and callable.
  has "$thread_line" '"method":"thread/resume"' || { fail_turn "$tid" "expected a resumed thread"; exit 0; }
  has "$thread_line" '"developerInstructions"' || { fail_turn "$tid" "LUNA instructions missing on resume"; exit 0; }
  if has "$thread_line" '"dynamicTools"'; then fail_turn "$tid" "resume must not replace dynamic tools"; exit 0; fi
  [ -n "$mcp_url" ] || { fail_turn "$tid" "LUNA MCP endpoint missing on resume"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-runtime-resume\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-resumed","turn":{"id":"t-runtime-resume"}}}'
  listed=$(mcp '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}')
  has "$listed" '"name":"luna_status"' || { fail_turn "$tid" "resumed MCP tools/list missing status tool"; exit 0; }
  response=$(mcp_tool 2 luna_status '{}' th-resumed t-runtime-resume call-resume)
  has "$response" '"isError":false' || { fail_turn "$tid" "resumed MCP tool call failed: $response"; exit 0; }
  emit '{"method":"turn/completed","params":{"threadId":"th-resumed","turn":{"id":"t-runtime-resume","status":"completed"}}}'
  ;;

*scenario:runtime-contract-cancelled*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-runtime-cancelled\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-runtime-cancelled"}}}'
  read -r interrupt_line || exit 1
  has "$interrupt_line" '"method":"turn/interrupt"' || { fail_turn "$tid" "expected interrupt"; exit 0; }
  denied_response=$(mcp_tool 70 luna_status '{}' th-1 t-runtime-cancelled call-cancelled)
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "cancelled MCP tool call was not denied"; exit 0; }
  emit '{"method":"item/tool/call","id":70,"params":{"threadId":"th-1","turnId":"t-runtime-cancelled","callId":"call-cancelled-legacy","tool":"luna_status","arguments":{}}}'
  read -r denied_response || exit 1
  has "$denied_response" '"success":false' || { fail_turn "$tid" "cancelled tool call was not denied"; exit 0; }
  emit "{\"id\":$(rid "$interrupt_line"),\"result\":{}}"
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-runtime-cancelled","status":"interrupted"}}}'
  ;;

*scenario:runtime-contract*)
  [ "$LUNA_THREAD_ID" = 'thread-a' ] || { fail_turn "$tid" "LUNA thread environment missing"; exit 0; }
  has "$thread_line" '"developerInstructions"' || { fail_turn "$tid" "LUNA instructions missing"; exit 0; }
  if has "$thread_line" '"dynamicTools"'; then fail_turn "$tid" "LUNA tools must not be frozen as dynamicTools"; exit 0; fi
  # Spawn wiring: loopback URL, token only in the environment, and Codex's own
  # MCP approval pre-approved for this server alone.
  case "$mcp_url" in http://127.0.0.1:*/mcp) ;; *) fail_turn "$tid" "LUNA MCP endpoint missing: $mcp_url"; exit 0 ;; esac
  [ -n "$LUNA_MCP_BEARER_TOKEN" ] || { fail_turn "$tid" "MCP token missing"; exit 0; }
  if has "$argv" "$LUNA_MCP_BEARER_TOKEN"; then fail_turn "$tid" "MCP token leaked into argv"; exit 0; fi
  has "$argv" 'mcp_servers.luna.bearer_token_env_var="LUNA_MCP_BEARER_TOKEN"' || { fail_turn "$tid" "bearer env var not configured"; exit 0; }
  has "$argv" 'mcp_servers.luna.default_tools_approval_mode="approve"' || { fail_turn "$tid" "Codex approval not scoped to luna"; exit 0; }
  has "$argv" 'mcp_servers.luna.tool_timeout_sec=86400' || { fail_turn "$tid" "LUNA tool timeout not raised"; exit 0; }
  unauthorized=$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$mcp_url" -H 'Authorization: Bearer wrong' -H 'Content-Type: application/json' --data '{"jsonrpc":"2.0","id":0,"method":"tools/list"}')
  [ "$unauthorized" = 401 ] || { fail_turn "$tid" "wrong MCP token was accepted: $unauthorized"; exit 0; }
  init=$(mcp '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fake-codex","version":"0"}}}')
  has "$init" '"protocolVersion":"2025-06-18"' || { fail_turn "$tid" "MCP initialize failed: $init"; exit 0; }
  listed=$(mcp '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}')
  has "$listed" '"name":"luna_status"' || { fail_turn "$tid" "status tool missing"; exit 0; }
  has "$listed" '"readOnlyHint":true' || { fail_turn "$tid" "status tool not read-only"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-runtime\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-runtime"}}}'
  denied_response=$(mcp_tool 59 luna_status '{}' foreign-thread t-runtime call-foreign)
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "foreign task tool call was not denied"; exit 0; }
  denied_response=$(mcp_tool 61 luna_status '{}' th-1 t-stale call-stale)
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "stale turn tool call was not denied"; exit 0; }
  denied_response=$(mcp '{"jsonrpc":"2.0","id":62,"method":"tools/call","params":{"name":"luna_status","arguments":{},"_meta":{"threadId":"th-1","x-codex-turn-metadata":{"turn_id":"t-runtime"}}}}')
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "missing call ID was not denied"; exit 0; }
  denied_response=$(mcp_tool 63 luna_status '{}' th-1 t-runtime '')
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "empty call ID was not denied"; exit 0; }
  denied_response=$(mcp '{"jsonrpc":"2.0","id":64,"method":"tools/call","params":{"name":"luna_status","arguments":{},"_meta":{"callId":"call-no-turn","threadId":"th-1"}}}')
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "missing turn metadata was not denied"; exit 0; }
  denied_response=$(mcp_tool 65 luna_archive '{}' th-1 t-runtime call-unattached)
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "unattached tool was not denied"; exit 0; }
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-runtime","item":{"id":"item-runtime","type":"mcpToolCall","server":"luna","tool":"luna_status","arguments":{},"status":"inProgress"}}}'
  tool_response=$(mcp_tool 60 luna_status '{}' th-1 t-runtime call-runtime)
  has "$tool_response" '"isError":false' || { fail_turn "$tid" "LUNA status call failed"; exit 0; }
  has "$tool_response" 'thread-a' || { fail_turn "$tid" "LUNA status response wrong"; exit 0; }
  denied_response=$(mcp_tool 66 luna_status '{}' th-1 t-runtime call-runtime)
  has "$denied_response" '"isError":true' || { fail_turn "$tid" "repeated call ID was not denied"; exit 0; }
  # The legacy dynamic-tool path shares the same per-turn call-ID ledger.
  emit '{"method":"item/tool/call","id":67,"params":{"threadId":"th-1","turnId":"t-runtime","callId":"call-runtime","tool":"luna_status","arguments":{}}}'
  read -r denied_response || exit 1
  has "$denied_response" '"success":false' || { fail_turn "$tid" "call ID replayed over item/tool/call was not denied"; exit 0; }
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-runtime","item":{"id":"item-runtime","type":"mcpToolCall","server":"luna","tool":"luna_status","arguments":{},"status":"completed","result":{"content":[{"type":"text","text":"{}"}],"isError":false}}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-runtime","status":"completed"}}}'
  # HTTP and stdout are separate channels: let the turn end land first. A
  # late call is denied, or refused once the session retired its endpoint.
  sleep 0.3
  late_response=$(mcp_tool 68 luna_status '{}' th-1 t-runtime call-late 2>/dev/null)
  if has "$late_response" '"isError":false'; then printf '%s\n' 'late call accepted' > "$CODEX_HOME/late-call-accepted"; fi
  ;;

*scenario:context-config-start*)
  cp "$CODEX_HOME/config.toml" "$CODEX_HOME/ordinary-start-config-observed"
  pwd > "$CODEX_HOME/ordinary-start-cwd"
  printf '%s\n' "$thread_line" > "$CODEX_HOME/ordinary-start-request"
  printf '%s\n' "$@" > "$CODEX_HOME/ordinary-start-argv"
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-context-config\"}}}"
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-context-config","status":"completed"}}}'
  ;;

*scenario:context-config-resume*)
  cp "$CODEX_HOME/config.toml" "$CODEX_HOME/ordinary-resume-config-observed"
  pwd > "$CODEX_HOME/ordinary-resume-cwd"
  printf '%s\n' "$thread_line" > "$CODEX_HOME/ordinary-resume-request"
  printf '%s\n' "$@" > "$CODEX_HOME/ordinary-resume-argv"
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-context-resume\"}}}"
  emit '{"method":"turn/completed","params":{"threadId":"context-resume","turn":{"id":"t-context-resume","status":"completed"}}}'
  ;;

*scenario:auto-context-complete*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-auto-context\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-auto-context"}}}'
  emit '{"method":"thread/tokenUsage/updated","params":{"threadId":"th-1","tokenUsage":{"last":{"totalTokens":95000},"modelContextWindow":100000}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-auto-context","item":{"id":"compact-auto","type":"contextCompaction"}}}'
  emit '{"method":"item/started","params":{"threadId":"child-auto","turnId":"child-turn","item":{"id":"compact-child","type":"contextCompaction"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-auto","turnId":"child-turn","item":{"id":"compact-child","type":"contextCompaction"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-auto-context","item":{"id":"compact-stale","type":"contextCompaction"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-auto-context","item":{"id":"compact-auto","type":"contextCompaction"}}}'
  emit '{"method":"thread/tokenUsage/updated","params":{"threadId":"th-1","tokenUsage":{"last":{"totalTokens":0},"modelContextWindow":100000}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-auto-context","status":"completed"}}}'
  ;;

*scenario:auto-context-stale-duplicate*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-stale-a\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-stale-a"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-stale-a","status":"completed"}}}'
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-current-b"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-stale-a","item":{"id":"compact-stale-a","type":"contextCompaction"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-current-b","item":{"id":"compact-current-b","type":"contextCompaction"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-current-b","item":{"id":"compact-current-b","type":"contextCompaction"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-current-b","item":{"id":"compact-current-b","type":"contextCompaction"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-current-b","item":{"id":"compact-current-b","type":"contextCompaction"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-current-b","status":"completed"}}}'
  ;;

*scenario:auto-context-terminal-failed*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-terminal-failed\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-terminal-failed"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-terminal-failed","item":{"id":"compact-terminal-failed","type":"contextCompaction"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-terminal-failed","status":"failed","error":{"message":"synthetic terminal failure"}}}}'
  sleep 1
  ;;

*scenario:auto-context-terminal-success-missing*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-terminal-success\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-terminal-success"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-terminal-success","item":{"id":"compact-terminal-success","type":"contextCompaction"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-terminal-success","status":"completed"}}}'
  sleep 1
  ;;

*scenario:auto-context-interrupted*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-auto-interrupted\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-auto-interrupted"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-auto-interrupted","item":{"id":"compact-interrupted","type":"contextCompaction"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-auto-interrupted","status":"interrupted"}}}'
  ;;

*scenario:auto-context-disconnect*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-auto-disconnect\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-auto-disconnect"}}}'
  emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-auto-disconnect","item":{"id":"compact-disconnect","type":"contextCompaction"}}}'
  exit 0
  ;;

*scenario:native-title*)
  has "$thread_line" '"ephemeral":false' || { fail_turn "$tid" "ordinary thread must be durable"; exit 0; }
  has "$native_name_line" '"name":"[LUNA] Native fixture"' || { fail_turn "$tid" "native LUNA title missing"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-native\"}}}"
  emit '{"method":"thread/name/updated","params":{"threadId":"th-1","name":"[LUNA] Native fixture"}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-native","status":"completed"}}}'
  ;;

*scenario:image-*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"item/started","params":{"threadId":"th-1","item":{"id":"image-1","type":"imageGeneration","status":"in_progress","result":""}}}'
  if has "$turnline" 'scenario:image-success'; then
    emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"id":"image-1","type":"imageGeneration","status":"completed","result":"INLINE_IMAGE_SENTINEL","savedPath":"/codex/generated_images/goblin.png"}}}'
  elif has "$turnline" 'scenario:image-failure'; then
    emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"id":"image-1","type":"imageGeneration","status":"failed","result":"INLINE_IMAGE_SENTINEL","failure":{"type":"usageLimitExceeded"}}}}'
  else
    emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"id":"image-1","type":"imageGeneration","status":"completed","result":"INLINE_IMAGE_SENTINEL"}}}'
  fi
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
  ;;

*scenario:title*)
  for want in '"sandbox":"read-only"' '"ephemeral":true' '"baseInstructions":"You generate session titles.' '"features.shell_tool":false' '"mcp_servers.test.enabled":false'; do
    has "$thread_line" "$want" || { fail_turn "$tid" "title restriction missing"; exit 0; }
  done
  has "$turnline" '"sandboxPolicy":{"type":"readOnly"}' || { fail_turn "$tid" "title turn not read-only"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"th-1","delta":"Fix Login Flow"}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  ;;


*scenario:reasoning*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"item/started","params":{"threadId":"th-1","item":{"id":"call_alpha","type":"subAgentActivity","kind":"spawned","agentThreadId":"child-1","agentPath":"/root/alpha"}}}'
  emit '{"method":"item/reasoning/summaryPartAdded","params":{"threadId":"th-1","itemId":"r1","summaryIndex":0}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"th-1","itemId":"r1","summaryIndex":0,"delta":"**Implementing file"}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"child-1","itemId":"r1","summaryIndex":0,"delta":"**Checking"}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"th-1","itemId":"r1","summaryIndex":0,"delta":" badges**"}}'
  emit '{"method":"item/reasoning/summaryPartAdded","params":{"threadId":"th-1","itemId":"r1","summaryIndex":1}}'
  emit '{"method":"item/reasoning/summaryPartAdded","params":{"threadId":"th-1","itemId":"r1","summaryIndex":1}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"th-1","itemId":"r1","summaryIndex":1,"delta":"**Preparing fixture screenshots**"}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"child-1","itemId":"r1","summaryIndex":0,"delta":" layout**"}}'
  emit '{"method":"item/reasoning/summaryPartAdded","params":{"threadId":"child-1","itemId":"r1","summaryIndex":1}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"child-1","itemId":"r1","delta":"Inspecting the output panel."}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"th-1","itemId":"r2","summaryIndex":0,"delta":"Checking the final result."}}'
  emit '{"method":"turn/completed","params":{"threadId":"child-1","turn":{"id":"ct-1"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  ;;

*scenario:publication*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"publication-turn\"}}}"
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"publication-answer","delta":"Publication turn completed"}}'
  emit '{"method":"turn/completed","params":{"turn":{"id":"publication-turn","status":"completed"}}}'
  ;;

*scenario:tool-history*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"tool-history-turn\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"tool-history-turn"}}}'
  emit '{"method":"item/started","params":{"item":{"id":"read-1","type":"commandExecution","command":"cat /tmp/demo.rs","commandActions":[{"type":"read","command":"cat /tmp/demo.rs","name":"demo.rs","path":"/tmp/demo.rs"}]}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"read-1","type":"commandExecution","status":"completed","exitCode":0,"aggregatedOutput":"old\n"}}}'
  emit '{"method":"item/started","params":{"item":{"id":"search-1","type":"commandExecution","command":"rg old /tmp/demo.rs","commandActions":[{"type":"search","command":"rg old /tmp/demo.rs","query":"old","path":"/tmp/demo.rs"}]}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"search-1","type":"commandExecution","status":"completed","exitCode":0,"aggregatedOutput":"1:old\n"}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"edit-1","type":"fileChange","status":"completed","changes":[{"path":"/tmp/demo.rs","kind":{"type":"update","move_path":null},"diff":"diff --git a//tmp/demo.rs b//tmp/demo.rs\n@@ -1 +1 @@\n-old\n+new\n"}]}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"create-1","type":"fileChange","status":"completed","changes":[{"path":"/tmp/created.txt","kind":{"type":"add"},"diff":"created by provider\n"}]}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"web-1","type":"webSearch","query":"Rust docs","action":{"type":"search","query":"Rust docs"},"results":[{"title":"Rust documentation","url":"https://doc.rust-lang.org/","snippet":"Learn Rust"}]}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"image-1","type":"imageView","path":"/tmp/demo.png"}}}'
  emit '{"method":"turn/completed","params":{"turn":{"id":"tool-history-turn","status":"completed"}}}'
  ;;

*scenario:happy*)
  # Verify the turn/start + thread/start params the harness must send.
  for want in '"method":"turn/start"' '"effort":"ultra"' '"model":"gpt-5.6-sol"' \
    '"sandboxPolicy":{' '"type":"workspaceWrite"' \
    '"approvalPolicy":"on-request"' '"approvalsReviewer":"user"' '"summary":"auto"' \
    '"serviceTier":"fast"'; do
    has "$turnline" "$want" || { fail_turn "$tid" "turn param missing: $want"; exit 0; }
  done
  for want in '"approvalPolicy":"on-request"' '"approvalsReviewer":"user"' '"sandbox":"workspace-write"' '"cwd":"/tmp"' \
    '"serviceTier":"fast"'; do
    has "$thread_line" "$want" || { fail_turn "$tid" "thread param missing: $want"; exit 0; }
  done
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  # Deltas — both field spellings must be accepted.
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"Hello"}}'
  emit '{"method":"item/reasoning/textDelta","params":{"itemId":"r1","textDelta":"thinking hard"}}'
  emit '{"method":"item/reasoning/summaryTextDelta","params":{"itemId":"r1","delta":"summary"}}'
  # Item lifecycles.
  emit '{"method":"item/started","params":{"item":{"id":"c1","type":"commandExecution","command":"ls -la"}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"c1","type":"commandExecution","command":"ls -la","status":"completed","exitCode":1,"aggregatedOutput":"ls: missing fixture\n"}}}'
  emit '{"method":"item/started","params":{"item":{"id":"f1","type":"fileChange","changes":[{"path":"/tmp/new.rs","kind":"add"}]}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"f1","type":"fileChange","status":"completed","changes":[{"path":"/tmp/new.rs","kind":"add"}]}}}'
  emit '{"method":"item/started","params":{"item":{"id":"mcp1","type":"mcpToolCall","server":"linear","tool":"search","arguments":{"q":"bug"}}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"mcp1","type":"mcpToolCall","server":"linear","tool":"search","status":"failed"}}}'
  emit '{"method":"item/started","params":{"item":{"id":"w1","type":"webSearch","query":"rust"}}}'
  emit '{"method":"item/completed","params":{"item":{"id":"w1","type":"webSearch","query":"rust"}}}'
  # Completion-only lifecycle: must still open AND close the tool call.
  emit '{"method":"item/completed","params":{"item":{"id":"td1","type":"todoList","items":[{"text":"a","completed":true},{"text":"b","completed":false}]}}}'
  # Streamed agentMessage: completed text must NOT re-emit.
  emit '{"method":"item/completed","params":{"item":{"id":"m1","type":"agentMessage","text":"Hello world"}}}'
  # Never-streamed agentMessage: completed text is the fallback delta.
  emit '{"method":"item/completed","params":{"item":{"id":"m2","type":"agentMessage","text":"unstreamed tail"}}}'
  # Unknown notification methods must be tolerated.
  emit '{"method":"some/unknownNotification","params":{"x":1}}'
  emit '{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":42,"outputTokens":7}}}}'
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  ;;

*scenario:child-identity*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  cat "$(dirname "$0")/codex/child-identity.jsonl"
  ;;

*scenario:v1-subagents*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  cat "$(dirname "$0")/codex/v1-subagents.jsonl"
  ;;

*scenario:v2-lifecycle*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  cat "$(dirname "$0")/codex/v2-lifecycle.jsonl"
  ;;

*scenario:child-unobserved*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"item/started","params":{"threadId":"th-1","item":{"type":"subAgentActivity","id":"spawn-open","kind":"started","agentThreadId":"child-open","agentPath":"/root/open"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"type":"subAgentActivity","id":"spawn-open","kind":"started","agentThreadId":"child-open","agentPath":"/root/open"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"child-open","itemId":"child-output","delta":"still working"}}'
  emit '{"method":"thread/status/changed","params":{"threadId":"child-open","status":{"type":"idle"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
  ;;

*scenario:child-late-result*)
  # The parent steers a child while the child's read still runs: the read
  # completes (re-announced, then its result) after the child already
  # answered the steer.
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"item/started","params":{"threadId":"th-1","item":{"type":"subAgentActivity","id":"spawn-late","kind":"started","agentThreadId":"child-late","agentPath":"/root/late"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"type":"subAgentActivity","id":"spawn-late","kind":"started","agentThreadId":"child-late","agentPath":"/root/late"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-late","item":{"type":"userMessage","id":"late-input","text":"Read the slow file"}}}'
  emit '{"method":"item/started","params":{"threadId":"child-late","item":{"type":"commandExecution","id":"slow-read","command":"cat slow.txt","commandActions":[{"type":"read","path":"slow.txt"}]}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"type":"subAgentActivity","id":"message-late","kind":"interacted","agentThreadId":"child-late","agentPath":"/root/late"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-late","item":{"type":"userMessage","id":"late-steer","text":"Also summarize it"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"child-late","itemId":"late-reply","delta":"CHILD_FOLLOW_UP"}}'
  emit '{"method":"item/completed","params":{"threadId":"child-late","item":{"type":"commandExecution","id":"slow-read","command":"cat slow.txt","commandActions":[{"type":"read","path":"slow.txt"}],"status":"completed","exitCode":0,"aggregatedOutput":"SLOW_READ_OUTPUT"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-late","item":{"type":"agentMessage","id":"late-reply","text":"CHILD_FOLLOW_UP"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"child-late","turn":{"id":"late-turn","status":"completed"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"type":"subAgentActivity","id":"late-completed","kind":"completed","agentThreadId":"child-late","agentPath":"/root/late"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"type":"agentMessage","id":"parent-answer","text":"parent answer"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
  ;;

*scenario:subagent*)
  # Multi-agent v2 child-thread routing: registration via subAgentActivity,
  # tagged child items, consumed child turn bookkeeping (must never settle
  # the parent turn), tagged Done on thread/closed.
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  # Parent spawn item registers the child (call id = the parent chip).
  emit '{"method":"item/started","params":{"threadId":"th-1","item":{"id":"call_alpha","type":"subAgentActivity","kind":"started","agentThreadId":"child-1","agentPath":"/root/alpha"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"id":"call_alpha","type":"subAgentActivity","kind":"started","agentThreadId":"child-1","agentPath":"/root/alpha"}}}'
  # The wire also emits subAgentActivity about the ROOT during collab runs:
  # no chip, no registration.
  emit '{"method":"item/started","params":{"threadId":"th-1","item":{"id":"call_root","type":"subAgentActivity","kind":"interacted","agentThreadId":"th-1","agentPath":"/root"}}}'
  # Child traffic: status is consumed; item lifecycles arrive tagged.
  emit '{"method":"thread/status/changed","params":{"threadId":"child-1","status":{"type":"running"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"child-1","itemId":"cm1","delta":"child says hi"}}'
  emit '{"method":"item/started","params":{"threadId":"child-1","item":{"id":"cs1","type":"commandExecution","command":"echo hi"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-1","item":{"id":"cs1","type":"commandExecution","command":"echo hi","status":"completed","exitCode":0}}}'
  # The parent steering the child (collab send_message): a userMessage item
  # on the CHILD thread — tagged UserMessage, emitted once (completed only).
  emit '{"method":"item/started","params":{"threadId":"child-1","item":{"id":"cu1","type":"userMessage","text":"also check the rebuild"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-1","item":{"id":"cu1","type":"userMessage","text":"also check the rebuild"}}}'
  # The child settles ITS turn — the parent turn must keep running.
  emit '{"method":"turn/completed","params":{"threadId":"child-1","turn":{"id":"ct-1"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"th-1","itemId":"m1","delta":"parent still going"}}'
  # Unknown method addressed to the child must fall through, not vanish.
  emit '{"method":"thread/somethingBrandNew","params":{"threadId":"child-1"}}'
  # Child closes → tagged terminal; the spawn chip resolves on the parent.
  emit '{"method":"thread/closed","params":{"threadId":"child-1"}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","item":{"id":"subagent-completed-alpha","type":"subAgentActivity","kind":"completed","agentThreadId":"child-1","agentPath":"/root/alpha"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  ;;

# NOTE: specific steer scenarios before steer — `case` takes the first match.
*scenario:terminal-race:*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"th-1","turnId":"t-1","itemId":"m1","delta":"terminal-race-ready"}}'
  read -r interruptline || exit 1
  has "$interruptline" '"method":"turn/interrupt"' || exit 1
  emit "{\"id\":$(rid "$interruptline"),\"result\":{}}"
  status=completed
  if has "$turnline" 'terminal-race:failed'; then status=failed; fi
  if has "$turnline" 'terminal-race:interrupted'; then status=interrupted; fi
  emit "{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"th-1\",\"turn\":{\"id\":\"t-1\",\"status\":\"$status\"}}}"
  ;;

*scenario:external-interruption*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"interrupted"}}}'
  ;;

*scenario:steer-record-lost-ack*|*scenario:steer-readback*|*scenario:steer-multiple-records*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  read -r steerline || exit 1
  has "$steerline" '"method":"turn/steer"' || exit 1
  client_id=$(cid "$steerline")
  case "$client_id" in native-correlated-steer|native-a) ;; *) exit 1 ;; esac
  if has "$turnline" 'scenario:steer-record-lost-ack'; then
    emit "{\"method\":\"item/started\",\"params\":{\"threadId\":\"th-1\",\"turnId\":\"t-1\",\"item\":{\"id\":\"u1\",\"type\":\"userMessage\",\"clientId\":\"$client_id\",\"content\":[]}}}"
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
  else
    emit "{\"id\":$(rid "$steerline"),\"result\":{\"turnId\":\"t-1\"}}"
    if has "$turnline" 'scenario:steer-multiple-records'; then
      read -r second || exit 1
      has "$second" '"clientUserMessageId":"native-b"' || exit 1
      emit "{\"id\":$(rid "$second"),\"result\":{\"turnId\":\"t-1\"}}"
      # Foreign/stale/missing identity must not consume either accepted input.
      emit '{"method":"item/started","params":{"threadId":"foreign","turnId":"t-1","item":{"type":"userMessage","clientId":"native-a"}}}'
      emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"stale","item":{"type":"userMessage","clientId":"native-a"}}}'
      emit '{"method":"item/started","params":{"threadId":"th-1","turnId":"t-1","item":{"type":"userMessage","text":"same prompt"}}}'
      for id in native-a native-a native-b native-b; do
        emit "{\"method\":\"item/started\",\"params\":{\"threadId\":\"th-1\",\"turnId\":\"t-1\",\"item\":{\"type\":\"userMessage\",\"clientId\":\"$id\"}}}"
      done
      emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
    else
      emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
      read -r readback || exit 1
      has "$readback" '"method":"thread/turns/list"' || exit 1
      has "$readback" '"sortDirection":"desc"' || exit 1
      turn_page "$readback" "[{\"id\":\"t-1\",\"status\":\"completed\",\"itemsView\":\"full\",\"error\":null,\"items\":[{\"type\":\"userMessage\",\"id\":\"u1\",\"clientId\":\"$client_id\",\"content\":[]}]}]"
    fi
  fi
  ;;

*scenario:steer-correlation-ambiguous*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  read -r steerline || exit 1
  sid=$(rid "$steerline")
  has "$steerline" '"method":"turn/steer"' || exit 1
  emit "{\"id\":$sid,\"result\":{\"turnId\":\"t-1\"}}"
  client_id=$(cid "$steerline")
  # None of these identity-free text echoes can prove which durable request
  # was incorporated: one is stale, two are duplicate same-turn content.
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-stale","item":{"id":"u-stale","type":"userMessage","text":"same prompt"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-1","item":{"id":"u-first","type":"userMessage","text":"same prompt"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-1","item":{"id":"u-duplicate","type":"userMessage","text":"same prompt"}}}'
  emit "{\"method\":\"item/started\",\"params\":{\"threadId\":\"th-1\",\"turnId\":\"t-1\",\"item\":{\"type\":\"userMessage\",\"clientId\":\"$client_id\"}}}"
  emit "{\"method\":\"item/completed\",\"params\":{\"threadId\":\"th-1\",\"turnId\":\"t-1\",\"item\":{\"type\":\"userMessage\",\"clientId\":\"$client_id\"}}}"
  emit '{"method":"item/agentMessage/delta","params":{"threadId":"th-1","turnId":"t-1","itemId":"m1","delta":"ambiguous-echoes-observed"}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  ;;

*scenario:steer-wrong-turn*|*scenario:steer-missing-turn*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  read -r steerline || exit 1
  sid=$(rid "$steerline")
  has "$steerline" '"method":"turn/steer"' || exit 1
  if has "$turnline" 'scenario:steer-wrong-turn'; then
    emit "{\"id\":$sid,\"result\":{\"turnId\":\"t-other\"}}"
  else
    emit "{\"id\":$sid,\"result\":{}}"
  fi
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  ;;

*scenario:steer-lost-response*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  read -r steerline || exit 1
  has "$steerline" '"method":"turn/steer"' || exit 1
  # The request arrived, but its response is deliberately lost. Ending the
  # wire forces a transport-unknown outcome; explicit steering must not retry.
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"request-arrived-response-lost"}}'
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  ;;

*scenario:steer-accepted-only*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  read -r steerline || exit 1
  sid=$(rid "$steerline")
  has "$steerline" '"method":"turn/steer"' || exit 1
  emit "{\"id\":$sid,\"result\":{\"turnId\":\"t-1\"}}"
  # Acceptance schedules input; without native evidence it stays unconfirmed.
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"accepted-without-incorporation"}}'
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  ;;

*scenario:steer-race*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  read -r steerline || exit 1
  sid=$(rid "$steerline")
  has "$steerline" '"method":"turn/steer"' ||
    { emit "{\"id\":$sid,\"result\":{}}"; emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"expected turn/steer"}}}}'; exit 0; }
  # The turn completed under the steer: reject, then announce completion.
  emit "{\"id\":$sid,\"error\":{\"code\":-32602,\"message\":\"turn already completed\"}}"
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  # The harness must fall back to a follow-up turn/start carrying the text.
  read -r followline || exit 1
  fid=$(rid "$followline")
  if has "$followline" '"method":"turn/start"' && has "$followline" 'redirect please'; then
    emit "{\"id\":$fid,\"result\":{\"turn\":{\"id\":\"t-2\"}}}"
    emit '{"method":"turn/started","params":{"turn":{"id":"t-2"}}}'
    emit '{"method":"item/agentMessage/delta","params":{"itemId":"m2","delta":"fallback"}}'
    emit '{"method":"turn/completed","params":{"turn":{"id":"t-2"}}}'
  else
    fail_turn "$fid" "expected fallback turn/start with steer text"
  fi
  ;;

*scenario:steer*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"first"}}'
  read -r steerline || exit 1
  sid=$(rid "$steerline")
  if has "$steerline" '"method":"turn/steer"' &&
    has "$steerline" '"expectedTurnId":"t-1"' &&
    has "$steerline" 'redirect please'; then
    emit "{\"id\":$sid,\"result\":{\"turnId\":\"t-1\"}}"
    client_id=$(cid "$steerline")
    emit "{\"method\":\"item/completed\",\"params\":{\"threadId\":\"th-1\",\"turnId\":\"t-1\",\"item\":{\"id\":\"u-steer\",\"type\":\"userMessage\",\"clientId\":\"$client_id\",\"content\":[]}}}"
    emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"steered"}}'
    emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  else
    emit "{\"id\":$sid,\"error\":{\"code\":-32600,\"message\":\"bad steer\"}}"
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"steer verification failed"}}}}'
  fi
  ;;

*scenario:approve*)
  has "$thread_line" '"approvalPolicy":"on-request"' ||
    { fail_turn "$tid" "thread approvalPolicy should be on-request"; exit 0; }
  has "$turnline" '"approvalPolicy":"on-request"' ||
    { fail_turn "$tid" "turn approvalPolicy should be on-request"; exit 0; }
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"id":101,"method":"item/commandExecution/requestApproval","params":{"itemId":"c1","command":"rm -rf /tmp/x"}}'
  read -r a1 || exit 1
  { has "$a1" '"id":101' && has "$a1" '"decision":"accept"'; } ||
    { emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"command approval not accepted"}}}}'; exit 0; }
  emit '{"id":102,"method":"item/fileChange/requestApproval","params":{"itemId":"f1","changes":[{"path":"/tmp/a.rs","kind":"update"}]}}'
  read -r a2 || exit 1
  { has "$a2" '"id":102' && has "$a2" '"decision":"accept"'; } ||
    { emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"file approval not accepted"}}}}'; exit 0; }
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  ;;

*scenario:decline*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"id":201,"method":"item/commandExecution/requestApproval","params":{"itemId":"c1","command":"rm -rf /"}}'
  read -r a1 || exit 1
  { has "$a1" '"id":201' && has "$a1" '"decision":"decline"'; } ||
    { emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"expected decline"}}}}'; exit 0; }
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  ;;

*scenario:interrupt*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"working"}}'
  read -r intline || exit 1
  iid=$(rid "$intline")
  if has "$intline" '"method":"turn/interrupt"' && has "$intline" '"turnId":"t-1"'; then
    emit "{\"id\":$iid,\"result\":{}}"
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"interrupted"}}}'
  else
    emit "{\"id\":$iid,\"result\":{}}"
    emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"expected turn/interrupt"}}}}'
  fi
  ;;

*scenario:wedge*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"working"}}'
  # Ignore turn/interrupt entirely — forces the SIGTERM escalation path.
  exec sleep 30
  ;;

*scenario:fail*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"failed","error":{"message":"boom"}}}}'
  ;;

*scenario:resumed-child*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-2\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"child-alpha","turn":{"id":"alpha-resumed"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-resumed","item":{"type":"subAgentActivity","id":"resumed-interaction","kind":"interacted","agentThreadId":"child-alpha","agentPath":"/root/alpha"}}}'
  emit '{"method":"item/completed","params":{"threadId":"child-alpha","item":{"type":"agentMessage","id":"resumed-answer","text":"resumed alpha"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"child-alpha","turn":{"id":"alpha-resumed","status":"completed"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-resumed","turn":{"id":"t-2","status":"completed"}}}'
  ;;

*scenario:goal-warm*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"t-1"}}}'
  emit '{"method":"item/completed","params":{"threadId":"th-1","turnId":"t-1","item":{"type":"agentMessage","id":"warm-msg","text":"ready"}}}'
  emit '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"t-1","status":"completed"}}}'
  # Metadata reads may share the warm connection between turns; answer them
  # as unsupported. A turn/start or turn/steer here is the bug under test.
  while read -r clearline; do
    has "$clearline" '"method":"thread/goal/clear"' && break
    [ -z "$CODEX_HOME" ] || printf '%s\n' "$clearline" >> "$CODEX_HOME/goal-warm-skipped"
    if has "$clearline" '"method":"turn/start"' || has "$clearline" '"method":"turn/steer"'; then
      emit '{"method":"error","params":{"threadId":"th-1","message":"warm goal must clear first, not turn/start or turn/steer"}}'
      exit 0
    fi
    if has "$clearline" '"id":'; then
      emit "{\"id\":$(rid "$clearline"),\"error\":{\"code\":-32601,\"message\":\"fake goal-warm: unsupported\"}}"
    fi
  done
  [ -n "$clearline" ] || exit 0
  goal_start "$clearline" || exit 0
  goal_turn th-1 goal-turn-2 'warm goal done'
  ;;

*scenario:resumed*)
  emit "{\"id\":$tid,\"result\":{\"turn\":{\"id\":\"t-1\"}}}"
  emit '{"method":"turn/started","params":{"turn":{"id":"t-1"}}}'
  emit '{"method":"turn/completed","params":{"turn":{"id":"t-1"}}}'
  ;;

*)
  fail_turn "$tid" "unknown scenario"
  ;;
esac
