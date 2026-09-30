#!/usr/bin/env node
// Read-only actual engine/adapter discovery E2E. Does not launch LUNA, create
// checkouts, install plugins, alter credentials or send model/user messages.
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';

const params = { refresh: true };
const mapping = { '--chat': 'chatId', '--space': 'spaceId', '--account': 'providerAccountId', '--target': 'targetDeviceId', '--path': 'path' };
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 2) {
  const field = mapping[args[index]];
  if (!field || !args[index + 1]) throw new Error('Use --chat ID or --space ID, optionally --account ID --target ID --path existingCheckout');
  params[field] = args[index + 1];
}
assert.ok(params.chatId || params.spaceId, 'An existing host-owned task or project is required');
assert.ok(process.env.LUNA_PICKER_EVIDENCE_DIR, 'Set LUNA_PICKER_EVIDENCE_DIR to a preserved directory outside the checkout');
const directory = process.env.LUNA_PICKER_EVIDENCE_DIR;
const ws = new WebSocket(`ws://127.0.0.1:${Number(process.env.LUNA_IPC_PORT || 59777)}`);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 20000);
    function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer); ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(String(frame.err))); else resolve(frame.ok);
    }
    ws.addEventListener('message', receive);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
const evidence = { evidenceKind: 'live-engine-native-discovery', params, observations: [], files: null, passed: false };
try {
  for (const refresh of [true, false]) {
    const catalog = await call('ListComposerCatalog', { ...params, refresh });
    evidence.observations.push(catalog);
    assert.match(catalog.scope, /^[0-9a-f]{64}$/);
    assert.equal(catalog.ttlMs, 300000);
    assert.ok(catalog.fetchedAtMs > 0);
    assert.equal(catalog.slices.length, 3);
    const seen = new Set();
    for (const row of catalog.entries) {
      assert.ok(row.name && row.id);
      const key = `${row.kind}:${row.path}`;
      assert.ok(!seen.has(key), 'Duplicate native identity'); seen.add(key);
      if (row.kind === 'skill') assert.ok(row.path.startsWith('/') || /^[A-Za-z]:[\\/]/.test(row.path));
      else assert.equal(row.path, `${row.kind === 'plugin' ? 'plugin' : 'app'}://${row.id}`);
    }
  }
  assert.equal(evidence.observations[0].scope, evidence.observations[1].scope, 'Stable execution identity changed during the scenario');
  const { refresh, providerAccountId, ...fileParams } = params;
  evidence.files = await call('SearchFiles', { ...fileParams, query: '' });
  assert.ok(Array.isArray(evidence.files), 'File @ discovery still returns its existing contract');
  if (process.env.LUNA_PICKER_ALLOW_UNAVAILABLE !== '1') {
    assert.ok(evidence.observations[0].slices.some(slice => slice.status !== 'unavailable'), 'No native slice was verified');
  }
  evidence.passed = true;
} catch (error) {
  evidence.error = error.message;
  process.exitCode = 1;
} finally {
  ws.close();
  await mkdir(directory, { recursive: true });
  const data = JSON.stringify(evidence, null, 2) + '\n';
  const path = join(directory, 'live-engine-catalog.json');
  await writeFile(path, data);
  const sha256 = createHash('sha256').update(data).digest('hex');
  await writeFile(join(directory, 'live-engine-catalog.sha256'), `${sha256}  live-engine-catalog.json\n`);
  console.log(JSON.stringify({ passed: evidence.passed, evidence: path, sha256,
    slices: evidence.observations[0]?.slices.map(({ kind, status }) => ({ kind, status })) }));
}
