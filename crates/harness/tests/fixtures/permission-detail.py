#!/usr/bin/env python3
"""Synthetic Codex peer: exact wire receipts, no model/provider/GUI claim."""
import json
import os
import sys

receipt = os.environ.get('LUNA_PERMISSION_RECEIPT')

def emit(value):
    print(json.dumps(value), flush=True)

def read():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(1)
    value = json.loads(line)
    if receipt:
        with open(receipt, 'a') as stream:
            stream.write(json.dumps(value) + '\n')
    return value

def result(request, value):
    emit({'id': request['id'], 'result': value})

init = read()
result(init, {'userAgent': 'permission-detail-fixture'})
read()  # initialized
thread = read()
result(thread, {'thread': {'id': 'permission-thread'}})
turn = read()
scenario = turn['params']['input'][0]['text']
result(turn, {'turn': {'id': 'permission-turn'}})
emit({'method': 'turn/started', 'params': {'threadId': 'permission-thread', 'turn': {'id': 'permission-turn'}}})
base = {'threadId': 'permission-thread', 'turnId': 'permission-turn', 'itemId': 'permission-item', 'startedAtMs': 1}
permission = {'network': {'enabled': True}, 'fileSystem': {'read': ['/tmp/read'], 'write': ['/tmp/write']}}
exec_rule = {'acceptWithExecpolicyAmendment': {'execpolicyAmendment': ['git', 'status']}}
network_rule = {'applyNetworkPolicyAmendment': {'networkPolicyAmendment': {'host': 'example.test', 'action': 'deny'}}}
if scenario.startswith('permission:'):
    method = 'item/permissions/requestApproval'
    params = dict(base, cwd='/tmp', environmentId='fixture-env', reason='Fixture permission', permissions=permission)
    choice = scenario.split(':')[1]
    expected = {'permissions': permission, 'scope': choice} if choice in ('turn', 'session') else {'permissions': {}, 'scope': 'turn'}
else:
    method = 'item/commandExecution/requestApproval'
    params = dict(base, kind='writeStdin', approvalId='opaque-callback', environmentId='fixture-env', cwd='/tmp', command='git status', reason='Fixture reason', networkApprovalContext={'host': 'example.test', 'protocol': 'https'}, commandActions=[{'type': 'unknown', 'command': 'git status'}], proposedExecpolicyAmendment=['git', 'status'], proposedNetworkPolicyAmendments=[{'host': 'example.test', 'action': 'deny'}], availableDecisions=['decline', 'accept', 'cancel', exec_rule, network_rule])
    choice = scenario.split(':')[1]
    expected = {'decision': {'once': 'accept', 'deny': 'decline', 'cancel': 'cancel', 'exec': exec_rule, 'network': network_rule, 'forged': 'decline'}[choice]}
if scenario.endswith(':stale'):
    params['turnId'] = 'old-turn'
    expected = {'permissions': {}, 'scope': 'turn'}
if scenario.endswith(':unknown'):
    params['permissions']['unknownCategory'] = {'enabled': True}
    expected = {'permissions': {}, 'scope': 'turn'}
emit({'id': 'request-opaque', 'method': method, 'params': params})
if scenario.endswith(':resolved'):
    emit({'method': 'serverRequest/resolved', 'params': {'threadId': 'permission-thread', 'requestId': 'request-opaque'}})
    emit({'method': 'turn/completed', 'params': {'threadId': 'permission-thread', 'turn': {'id': 'permission-turn', 'status': 'completed'}}})
    raise SystemExit(0)
answer = read()
if answer.get('result') != expected:
    emit({'method': 'turn/failed', 'params': {'threadId': 'permission-thread', 'turn': {'id': 'permission-turn', 'error': {'message': f'Expected {expected!r}; received {answer!r}'}}}})
else:
    emit({'method': 'turn/completed', 'params': {'threadId': 'permission-thread', 'turn': {'id': 'permission-turn', 'status': 'completed'}}})
