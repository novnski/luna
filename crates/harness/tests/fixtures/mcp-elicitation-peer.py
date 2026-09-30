"""Synthetic stdio peer; launched after fake-codex's actual handshake."""
import json
import sys

scenario = sys.argv[1].split('scenario:mcp-form:', 1)[1].split('"', 1)[0]

def emit(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)

schema = {'type': 'object', 'required': ['name', 'count', 'enabled', 'color', 'tags'], 'properties': {
    'name': {'type': 'string', 'minLength': 1, 'maxLength': 16},
    'count': {'type': 'integer', 'minimum': 0, 'maximum': 4},
    'enabled': {'type': 'boolean'},
    'color': {'type': 'string', 'oneOf': [{'const': 'blue', 'title': 'Blue'}, {'const': 'red', 'title': 'Red'}]},
    'tags': {'type': 'array', 'minItems': 1, 'items': {'anyOf': [{'const': 'a', 'title': 'Alpha'}, {'const': 'b', 'title': 'Beta'}]}},
}}
params = {'serverName': 'fixture', 'threadId': 'th-1', 'turnId': 't-mcp', 'mode': 'form', 'message': 'Fixture form', 'requestedSchema': schema}
declined = {'decline', 'foreign', 'bad-schema', 'foreign-thread', 'null-turn', 'bad-default', 'bad-required', 'too-many', 'unsupported-mode', 'url-bad-scheme', 'url-credentials'}
if scenario == 'foreign': params['turnId'] = 'foreign'
if scenario == 'foreign-thread': params['threadId'] = 'foreign'
if scenario == 'null-turn': params['turnId'] = None
if scenario == 'bad-default': schema['properties']['count']['default'] = 'wrong-type'
if scenario == 'bad-required': schema['required'].append('missing')
if scenario == 'too-many': schema['properties'].update({f'extra{i}': {'type': 'string'} for i in range(17)})
if scenario == 'unsupported-mode': params['mode'] = 'openai/userVerification'
if scenario == 'defaults':
    for name, value in {'name': 'Ada', 'count': 0, 'enabled': False, 'color': 'blue', 'tags': ['a']}.items(): schema['properties'][name]['default'] = value
    schema['properties']['optional'] = {'type': 'string'}
if scenario == 'formats':
    for name, fmt in {'email': 'email', 'date': 'date', 'time': 'date-time', 'uri': 'uri'}.items(): schema['properties'][name] = {'type': 'string', 'format': fmt}
    schema['properties']['decimal'] = {'type': 'number', 'minimum': 1, 'maximum': 2}
if scenario == 'bad-schema':
    schema['properties']['name']['pattern'] = 'unsupported'
if scenario.startswith('url-'):
    params = {**params, 'mode': 'url', 'elicitationId': 'opaque-flow', 'url': 'https://example.invalid/authorize?token=private-url-token'}
    params.pop('requestedSchema')
    if scenario == 'url-bad-scheme': params['url'] = 'javascript:alert(1)'
    if scenario == 'url-credentials': params['url'] = 'https://user:private-url-token@example.invalid/'

count = 2 if scenario == 'concurrent' else 1
for index in range(count):
    emit({'id': f'mcp-{index}', 'method': 'mcpServer/elicitation/request', 'params': params})
if scenario == 'duplicate': emit({'id': 'mcp-0', 'method': 'mcpServer/elicitation/request', 'params': params})
seen = set()
for line in sys.stdin:
    frame = json.loads(line)
    if frame.get('id') not in {f'mcp-{index}' for index in range(count)}:
        continue
    result = frame.get('result', {})
    expected = 'cancel' if 'cancel' in scenario else 'decline' if scenario in declined else 'accept'
    assert result.get('action') == expected, (scenario, result)
    if expected == 'accept' and not scenario.startswith('url-'):
        expected_content = {'name': 'Ada', 'count': 0, 'enabled': False, 'color': 'blue', 'tags': ['a']}
        if scenario == 'formats': expected_content.update({'email': 'ada@example.invalid', 'date': '2026-09-30', 'time': '2026-09-30T12:34:56Z', 'uri': 'https://example.invalid/path', 'decimal': 1.25})
        assert result['content'] == expected_content, result
    if expected != 'accept' or scenario.startswith('url-'):
        assert result.get('content') is None, result
    seen.add(frame['id'])
    if len(seen) == count:
        break
assert len(seen) == count
emit({'method': 'item/agentMessage/delta', 'params': {'threadId': 'th-1', 'turnId': 't-mcp', 'itemId': 'verified', 'delta': 'mcp-fixture-verified'}})
emit({'method': 'turn/completed', 'params': {'threadId': 'th-1', 'turn': {'id': 't-mcp', 'status': 'completed'}}})
