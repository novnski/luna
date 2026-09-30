#!/usr/bin/env python3
"""Deterministic stdio peer: records actual adapter requests, never a real model."""
import json
import pathlib
import sys
import time

log = None
cwd = None
turn_count = 0
turn_id = None

def emit(value):
    print(json.dumps(value), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    params = request.get('params', {})
    if method in ('thread/start', 'thread/resume'):
        cwd = params['cwd']
        log = pathlib.Path(cwd) / 'native-inputs-rpc.jsonl'
    if log is not None:
        with log.open('a') as output:
            output.write(json.dumps(request) + '\n')
    if method == 'initialized':
        continue
    result = {}
    fallback = method == 'turn/steer' and any(item.get('type') == 'text' and 'fallback-native' in item.get('text', '') for item in params.get('input', []))
    if method == 'initialize':
        result = {'userAgent': 'native-inputs-fixture'}
    elif method in ('thread/start', 'thread/resume'):
        result = {'thread': {'id': 'native-input-thread'}}
    elif method == 'account/read':
        delay = pathlib.Path(cwd) / 'catalog-delay' if cwd else None
        if delay and delay.is_file():
            time.sleep(float(delay.read_text()))
        result = {'account': {'type': 'apiKey'}}
    elif method == 'thread/read':
        result = {'thread': {'id': 'native-input-thread', 'disabledPluginIds': []}}
    elif method == 'plugin/installed':
        result = {'marketplaces': [{'plugins': [{'id':'native-plugin-id', 'name':'Same name', 'installed':True, 'enabled':True}]}]}
    elif method == 'app/installed':
        result = {'apps': [{'id':'native-app-id', 'runtimeName':'Same name', 'enabled':True, 'callable':True}]}
    elif method == 'app/read':
        result = {'apps': [{'id':'native-app-id', 'name':'Same name'}]}
    elif method == 'skills/list':
        result = {'data': [{'cwd':cwd, 'errors':[], 'skills':[{'name':'fixture', 'path':str(pathlib.Path(cwd)/'SKILL.md'), 'enabled':True}]}]}
    elif method == 'turn/start':
        turn_count += 1
        turn_id = f'native-input-turn-{turn_count}'
        result = {'turn': {'id': turn_id}}
    elif method == 'turn/steer':
        result = {'turnId': turn_id}
    if fallback:
        emit({'id':request['id'], 'error':{'code':-32602, 'message':'correlated fallback rejection'}})
    else:
        emit({'id': request['id'], 'result': result})
    if method == 'turn/start':
        emit({'method': 'item/agentMessage/delta', 'params': {'threadId': 'native-input-thread', 'turnId': turn_id, 'itemId': 'ready', 'delta': 'fixture ready'}})
        if any(item.get('type') == 'text' and 'fallback-native' in item.get('text', '') for item in params.get('input', [])):
            emit({'method': 'turn/completed', 'params': {'threadId': 'native-input-thread', 'turn': {'id': turn_id, 'status': 'completed'}}})
    elif method == 'turn/steer':
        if params.get('clientUserMessageId') and not fallback:
            emit({'method': 'item/completed', 'params': {'threadId': 'native-input-thread', 'turnId': turn_id, 'item': {'type': 'userMessage', 'id': 'fixture-steer-user', 'clientId': params['clientUserMessageId'], 'content': params.get('input', [])}}})
        emit({'method': 'turn/completed', 'params': {'threadId': 'native-input-thread', 'turn': {'id': turn_id, 'status': 'completed'}}})
