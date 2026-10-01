#!/usr/bin/env python3
"""Deterministic Codex app-server peer for the codex-adapter fixes.

Frames follow codex-rs (ceea67163f, re-checked at rust-v0.159.0):
- sub-agent approvals carry the CHILD threadId/turnId
  (app-server/src/bespoke_event_handling.rs:785);
- MCP tool-call approvals are empty-schema form elicitations with
  `_meta.codex_approval_kind:"mcp_tool_call"` (core/src/mcp_tool_call.rs:1980-2105)
  answered like the TUI (tui/src/bottom_pane/mcp_server_elicitation.rs:1158-1185);
- `error {willRetry:true}` (bespoke_event_handling.rs:1054-1070), `warning`
  (v2/notification.rs:31-36), `item/mcpToolCall/progress` (v2/mcp.rs:333-338);
- `item/tool/requestUserInput` questions (v2/item.rs:1721-1759).
No model, provider or GUI is involved.
"""
import json
import os
import select
import sys
import time

receipt = os.environ.get('LUNA_CODEX_ADAPTER_RECEIPT')
ROOT = 'root-thread'
ROOT_TURN = 'root-turn'
CHILD = 'child-alpha'
CHILD_TURN = 'child-turn-1'


def emit(value):
    print(json.dumps(value), flush=True)


def log(direction, value):
    if receipt:
        with open(receipt, 'a') as stream:
            stream.write(json.dumps({'dir': direction, 'msg': value}) + '\n')


_pending = b''


def read(timeout=None):
    """One JSON line from stdin. Unbuffered on the fd: Python's buffered
    `sys.stdin` can hold a complete line that `select` cannot see."""
    global _pending
    deadline = None if timeout is None else time.monotonic() + timeout
    while b'\n' not in _pending:
        if deadline is not None:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([0], [], [], left)[0]:
                return None
        chunk = os.read(0, 65536)
        if not chunk:
            raise SystemExit(0)
        _pending += chunk
    line, _pending = _pending.split(b'\n', 1)
    value = json.loads(line)
    log('in', value)
    return value


def send(value):
    log('out', value)
    emit(value)


def result(request, value):
    send({'id': request['id'], 'result': value})


def fail(message):
    send({'method': 'turn/completed', 'params': {'threadId': ROOT, 'turn': {
        'id': ROOT_TURN, 'status': 'failed', 'error': {'message': message}}}})
    raise SystemExit(0)


def done():
    send({'method': 'item/agentMessage/delta', 'params': {
        'threadId': ROOT, 'turnId': ROOT_TURN, 'itemId': 'verified', 'delta': 'peer-verified'}})
    send({'method': 'turn/completed', 'params': {'threadId': ROOT, 'turn': {'id': ROOT_TURN, 'status': 'completed'}}})


def responses(ids, timeout=8):
    """Collect responses for exactly these request ids (any order)."""
    got = {}
    while len(got) < len(ids):
        frame = read(timeout)
        if frame is None:
            fail(f'missing responses for {sorted(set(ids) - set(got))}')
        if frame.get('id') in ids and 'method' not in frame:
            got[frame['id']] = frame.get('result')
    return got


def silent(ids, timeout=1.0):
    """No response may arrive for these ids (resolved or turn ended)."""
    while True:
        frame = read(timeout)
        if frame is None:
            return
        if frame.get('id') in ids and 'method' not in frame:
            fail(f'closed request {frame["id"]} was answered: {frame!r}')


def spawn_child():
    item = {'type': 'subAgentActivity', 'id': 'spawn-alpha', 'kind': 'started',
            'agentThreadId': CHILD, 'agentPath': '/root/alpha'}
    send({'method': 'item/started', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN, 'item': item}})
    send({'method': 'item/completed', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN, 'item': item}})
    send({'method': 'turn/started', 'params': {'threadId': CHILD, 'turn': {
        'id': CHILD_TURN, 'items': [], 'itemsView': 'notLoaded', 'status': 'inProgress',
        'error': None, 'startedAt': 1790000000, 'completedAt': None, 'durationMs': None}}})
    send({'method': 'item/started', 'params': {'threadId': CHILD, 'turnId': CHILD_TURN, 'item': {
        'type': 'commandExecution', 'id': 'call-child-1', 'command': 'touch alpha.txt', 'cwd': '/tmp',
        'status': 'inProgress', 'commandActions': [], 'aggregatedOutput': None, 'exitCode': None}}})


def command_approval(thread, turn):
    return {'kind': 'command', 'threadId': thread, 'turnId': turn, 'itemId': 'call-child-1',
            'startedAtMs': 1790000000000, 'approvalId': None, 'environmentId': None,
            'reason': 'needs write access', 'networkApprovalContext': None,
            'command': 'touch alpha.txt', 'cwd': '/tmp',
            'commandActions': [{'type': 'unknown', 'command': 'touch alpha.txt'}],
            'additionalPermissions': None, 'proposedExecpolicyAmendment': None,
            'proposedNetworkPolicyAmendments': None, 'availableDecisions': ['accept', 'cancel']}


def child_turn_completed():
    send({'method': 'turn/completed', 'params': {'threadId': CHILD, 'turn': {
        'id': CHILD_TURN, 'items': [], 'itemsView': 'notLoaded', 'status': 'completed', 'error': None}}})


init = read()
result(init, {'userAgent': 'codex-adapter-fixture'})
read()  # initialized
thread = read()
result(thread, {'thread': {'id': ROOT, 'historyMode': 'paginated'}})
turn = read()
# Engine dispatch may name the thread first; startup reads the goal.
while turn.get('method') != 'turn/start':
    if 'id' not in turn:
        raise SystemExit(1)
    result(turn, {'goal': None} if turn.get('method') == 'thread/goal/get' else {})
    turn = read()
scenario = turn['params']['input'][0]['text']
result(turn, {'turn': {'id': ROOT_TURN, 'items': [], 'itemsView': 'notLoaded', 'status': 'inProgress'}})
send({'method': 'turn/started', 'params': {'threadId': ROOT, 'turn': {'id': ROOT_TURN, 'status': 'inProgress'}}})

if scenario == 'initial-client-id':
    client_id = turn['params'].get('clientUserMessageId')
    if client_id != 'luna-user-message-1':
        fail(f'initial clientUserMessageId was {client_id!r}')
    user = {'type': 'userMessage', 'id': 'u-initial', 'clientId': client_id,
            'content': [{'type': 'text', 'text': scenario, 'text_elements': []}]}
    send({'method': 'item/started', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN, 'item': user}})
    send({'method': 'item/completed', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN, 'item': user}})
    done()

elif scenario == 'subagent:command':
    spawn_child()
    # Unknown thread and a stale child turn are declined without a card.
    send({'id': 'ghost-1', 'method': 'item/commandExecution/requestApproval',
          'params': command_approval('ghost-thread', 'ghost-turn')})
    send({'id': 'stale-1', 'method': 'item/commandExecution/requestApproval',
          'params': command_approval(CHILD, 'child-turn-0')})
    send({'id': 'child-approval', 'method': 'item/commandExecution/requestApproval',
          'params': command_approval(CHILD, CHILD_TURN)})
    # A child MCP prompt with a null turn binds to the child's live turn.
    send({'id': 'child-mcp', 'method': 'mcpServer/elicitation/request', 'params': {
        'threadId': CHILD, 'turnId': None, 'serverName': 'fixture', 'mode': 'form', '_meta': None,
        'message': 'Alpha needs a name',
        'requestedSchema': {'type': 'object', 'required': ['name'],
                            'properties': {'name': {'type': 'string', 'minLength': 1}}}}})
    got = responses({'ghost-1', 'stale-1', 'child-approval', 'child-mcp'})
    expected = {'ghost-1': {'decision': 'decline'}, 'stale-1': {'decision': 'decline'},
                'child-approval': {'decision': 'accept'},
                'child-mcp': {'action': 'accept', 'content': {'name': 'Ada'}}}
    if got != expected:
        fail(f'expected {expected!r}; received {got!r}')
    child_turn_completed()
    done()

elif scenario == 'subagent:closed':
    spawn_child()
    send({'id': 'child-resolved', 'method': 'item/commandExecution/requestApproval',
          'params': command_approval(CHILD, CHILD_TURN)})
    send({'id': 'child-ended', 'method': 'item/fileChange/requestApproval', 'params': {
        'threadId': CHILD, 'turnId': CHILD_TURN, 'itemId': 'patch-child-1', 'startedAtMs': 1790000000000,
        'reason': None, 'grantRoot': None}})
    # Let both cards reach the client before closing them natively.
    silent({'child-resolved', 'child-ended'}, timeout=1.0)
    send({'method': 'serverRequest/resolved', 'params': {'threadId': CHILD, 'requestId': 'child-resolved'}})
    child_turn_completed()
    silent({'child-resolved', 'child-ended'}, timeout=1.5)
    done()

elif scenario.startswith('mcp-tool:'):
    choice = scenario.split(':', 1)[1]
    persist = 'session' if choice == 'forged' else ['session', 'always']
    send({'id': 'tool-approval', 'method': 'mcpServer/elicitation/request', 'params': {
        'threadId': ROOT, 'turnId': ROOT_TURN, 'serverName': 'fixture', 'mode': 'form',
        '_meta': {'codex_approval_kind': 'mcp_tool_call', 'persist': persist,
                  'tool_title': 'Create issue', 'tool_description': 'Creates a tracker issue',
                  'tool_params': {'title': 'Bug', 'labels': ['p1']},
                  'tool_params_display': [{'name': 'title', 'value': 'Bug', 'display_name': 'Title'},
                                          {'name': 'labels', 'value': ['p1'], 'display_name': 'Labels'}]},
        'message': 'Allow the fixture MCP server to run tool "create_issue"?',
        'requestedSchema': {'type': 'object', 'properties': {}}}})
    got = responses({'tool-approval'})['tool-approval']
    expected = {
        'once': {'action': 'accept', 'content': None},
        'session': {'action': 'accept', 'content': None, '_meta': {'persist': 'session'}},
        'always': {'action': 'accept', 'content': None, '_meta': {'persist': 'always'}},
        'deny': {'action': 'decline'},
        'cancel': {'action': 'cancel'},
        'forged': {'action': 'decline'},
    }[choice]
    if got != expected:
        fail(f'expected {expected!r}; received {got!r}')
    done()

elif scenario == 'notices':
    spawn_child()
    send({'method': 'error', 'params': {'error': {'message': 'Reconnecting... 1/5', 'codexErrorInfo': None,
          'additionalDetails': 'stream disconnected before completion'}, 'willRetry': True,
          'threadId': ROOT, 'turnId': ROOT_TURN}})
    # Hold the retry so an engine observer can see the transient notice.
    time.sleep(1.0)
    send({'method': 'error', 'params': {'error': {'message': 'Reconnecting... 2/5', 'codexErrorInfo': None,
          'additionalDetails': None}, 'willRetry': True, 'threadId': CHILD, 'turnId': CHILD_TURN}})
    send({'method': 'warning', 'params': {'threadId': ROOT, 'message': 'Model rerouted to gpt-6.1-sol'}})
    tool = {'type': 'mcpToolCall', 'id': 'mcp-call-1', 'server': 'fixture', 'tool': 'slow_job',
            'status': 'inProgress', 'arguments': {}, 'result': None, 'error': None}
    send({'method': 'item/started', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN, 'item': tool}})
    send({'method': 'item/mcpToolCall/progress', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN,
          'itemId': 'mcp-call-1', 'message': 'Indexed 40 of 100 files'}})
    send({'method': 'item/completed', 'params': {'threadId': ROOT, 'turnId': ROOT_TURN, 'item': {
        **tool, 'status': 'completed', 'result': {'content': [{'type': 'text', 'text': 'indexed'}]}}}})
    # A non-retrying error stays an error.
    send({'method': 'error', 'params': {'error': {'message': 'quota exhausted', 'codexErrorInfo': None,
          'additionalDetails': None}, 'willRetry': False, 'threadId': ROOT, 'turnId': ROOT_TURN}})
    child_turn_completed()
    done()

elif scenario == 'request-input':
    send({'id': 'ask-1', 'method': 'item/tool/requestUserInput', 'params': {
        'threadId': ROOT, 'turnId': ROOT_TURN, 'itemId': 'ask-item', 'isBlocking': True, 'autoResolutionMs': None,
        'questions': [
            {'id': 'q-color', 'header': 'Color', 'question': 'Pick a color', 'isOther': False, 'isSecret': False,
             'options': [{'label': 'Blue', 'description': 'Calm'}, {'label': 'Red', 'description': 'Loud'}]},
            {'id': 'q-token', 'header': 'Token', 'question': 'Paste the token', 'isOther': True,
             'isSecret': True, 'options': None},
        ]}})
    got = responses({'ask-1'})['ask-1']
    expected = {'answers': {'q-color': {'answers': ['Blue']}, 'q-token': {'answers': ['s3cret']}}}
    if got != expected:
        fail(f'expected {expected!r}; received {got!r}')
    done()

else:
    fail(f'unknown scenario {scenario!r}')
