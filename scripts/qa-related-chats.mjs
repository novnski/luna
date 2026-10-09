#!/usr/bin/env node
// E2E against an isolated headless LUNA process, real SQLite/documents/RPC,
// and the Mock provider. Native-provider and GUI evidence are separate.
// Failure cases: docs/reference/related-chats-redesign.md (recorded first).
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const binary = process.env.LUNA_RELATED_QA_BINARY || path.join(root, 'target/debug/luna');
const outputRoot = process.env.LUNA_RELATED_QA_EVIDENCE;
assert.ok(outputRoot && path.isAbsolute(outputRoot), 'Set LUNA_RELATED_QA_EVIDENCE to an absolute evidence directory');
fs.mkdirSync(outputRoot, { recursive: true });
const evidence = fs.mkdtempSync(path.join(outputRoot, 'engine-'));
const data = path.join(evidence, 'data');
const project = path.join(evidence, 'project');
fs.mkdirSync(data); fs.mkdirSync(project);
const log = fs.createWriteStream(path.join(evidence, 'engine.log'));
const port = await new Promise(resolve => {
  const server = net.createServer(); server.listen(0, '127.0.0.1', () => {
    const port = server.address().port; server.close(() => resolve(port));
  });
});
let engine, ws, nextId = 0, failure;
const checks = [], creationMs = [];
const save = (name, value) => fs.writeFileSync(path.join(evidence, `${name}.json`), JSON.stringify(value, null, 2) + '\n');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function start() {
  const env = { ...process.env, LUNA_DATA_DIR: data, LUNA_IPC_PORT: String(port), LUNA_HARNESS: 'mock', LUNA_EDGE_URL: '' };
  for (const name of ['LUNA_EDGE_TOKEN', 'LUNA_WORKOS_CLIENT_ID', 'LUNA_ORG_ID']) delete env[name];
  engine = spawn(binary, ['headless'], { cwd: root, env, stdio: ['ignore', 'pipe', 'pipe'] });
  engine.stdout.pipe(log, { end: false }); engine.stderr.pipe(log, { end: false });
  for (let i = 0; i < 100; i++) {
    assert.equal(engine.exitCode, null, 'engine exited during startup');
    try {
      ws = new WebSocket(`ws://127.0.0.1:${port}`);
      await new Promise((resolve, reject) => {
        ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true });
      });
      return;
    } catch { ws?.close(); await delay(100); }
  }
  throw new Error('IPC startup timed out');
}
async function stop() {
  ws?.close(); ws = undefined;
  if (!engine || engine.exitCode !== null) return;
  const own = engine;
  const exited = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('isolated engine did not exit gracefully')), 15000);
    own.once('exit', () => { clearTimeout(timer); resolve(); });
  });
  own.kill('SIGTERM'); await exited; engine = undefined;
}
function call(method, params = {}) {
  const socket = ws, id = ++nextId;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { socket.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 20000);
    function receive(event) {
      for (const raw of String(event.data).split('\n').filter(Boolean)) {
        const frame = JSON.parse(raw);
        if (frame.id !== id || !['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) continue;
        clearTimeout(timer); socket.removeEventListener('message', receive);
        if (frame.err) reject(new Error(JSON.stringify(frame.err)));
        else {
          if (Object.hasOwn(frame, 'item')) socket.send(JSON.stringify({ id, cancel: true }));
          resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
        }
      }
    }
    socket.addEventListener('message', receive); socket.send(JSON.stringify({ id, method, params }));
  });
}
async function until(fn) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) { const value = await fn(); if (value) return value; await delay(100); }
  throw new Error('readback timed out');
}
async function check(name, fn) { await fn(); checks.push(name); console.log(`PASS ${name}`); }
async function create(parentId, requestId = crypto.randomUUID()) {
  const start = performance.now();
  const result = await call('CreateSideChat', { parentId, requestId });
  creationMs.push(performance.now() - start);
  assert.equal(result.meta.state, 'active'); assert.equal(result.meta.expiresAtMs, 0); assert.equal(result.lease, null);
  return result;
}
async function list(parentId) {
  const items = []; let before;
  do { const page = await call('ListSideChats', { parentId, before }); items.push(...page.items); before = page.before; } while (before !== null);
  return items;
}
async function send(id, messageId, text) {
  await call('SendSideChat', { id, messageId, text });
  return until(async () => {
    const page = await call('ReadSideChat', { id });
    return !page.working && page.entries.some(entry => entry.role === 'assistant') && page;
  });
}

const parent = crypto.randomUUID(), space = crypto.randomUUID(), ids = [];
const config = { harness: 'mock', model: 'mock-model', sandbox: 'workspace-write', permissionMode: 'auto', effectivePermissionMode: 'auto', modelOptions: {} };
try {
  await start();
  const info = await call('EngineInfo'); save('engine-info', info);
  assert.ok(info.capabilities.includes('side-chat-persistence-v1'));
  await call('Mutate', { op: 'createSpace', spaceId: space, deviceId: info.deviceId, path: project });
  await call('Mutate', { op: 'createChat', chatId: parent, spaceId: space, cwd: project, config });
  const original = await create(parent); ids.push(original.meta.id); save('created', original);
  await check('same creation request is idempotent', async () => {
    const retry = await create(parent, original.meta.id); assert.equal(retry.meta.id, original.meta.id);
    assert.equal((await list(parent)).length, 1);
  });
  await check('changed creation payload conflicts', async () => {
    await assert.rejects(call('CreateSideChat', { parentId: parent, requestId: original.meta.id, forceSnapshot: true }));
  });
  await check('six simultaneously active side chats need no lease', async () => {
    for (let i = 0; i < 5; i++) ids.push((await create(parent)).meta.id);
    assert.equal((await list(parent)).filter(meta => meta.state === 'active').length, 6);
  });
  await check('side turn is isolated and duplicate send is not replayed', async () => {
    const page = await send(ids[0], 'first-side-message', 'SIDE_E2E_FIRST'); save('first-turn', page);
    await call('SendSideChat', { id: ids[0], messageId: 'first-side-message', text: 'SIDE_E2E_FIRST' });
    const again = await call('ReadSideChat', { id: ids[0] });
    assert.equal(again.entries.filter(entry => entry.role === 'user').length, 1);
    await assert.rejects(call('SendSideChat', { id: ids[0], messageId: 'first-side-message', text: 'DIFFERENT' }));
    const parentHistory = await call('WatchDocMessages', { chatId: parent });
    assert.ok(!JSON.stringify(parentHistory).includes('SIDE_E2E_FIRST'));
  });
  await check('Stop keeps the side chat writable', async () => {
    await call('StopSideChat', { id: ids[0] });
    assert.equal((await call('ReadSideChat', { id: ids[0] })).meta.state, 'active');
  });
  await check('no heartbeat is needed past the old 30-second lease', async () => {
    const started = performance.now(); await delay(32000);
    const page = await call('ReadSideChat', { id: ids[0] });
    assert.equal(page.meta.state, 'active'); save('no-heartbeat', { elapsedMs: performance.now() - started, page });
  });
  await check('all side rows and identities are published for unopened-parent counts', async () => {
    const rows = (await call('WatchChats')).filter(chat => chat.sideChatParentId === parent);
    assert.equal(rows.length, 6); assert.ok(rows.every(chat => chat.spaceId === space && chat.sideChatState === 'active'));
    save('watch-before-restart', rows);
  });
  await stop(); await start();
  await check('restart preserves active chats and transcript without replay', async () => {
    const page = await call('ReadSideChat', { id: ids[0] }); save('after-restart', page);
    assert.equal(page.meta.state, 'active'); assert.equal(page.entries.filter(entry => entry.role === 'user').length, 1);
    assert.equal((await list(parent)).filter(meta => meta.state === 'active').length, 6);
    const resumed = await send(ids[0], 'after-restart', 'SIDE_E2E_RESTART'); save('continued-after-restart', resumed);
    assert.equal(resumed.entries.filter(entry => entry.role === 'user').length, 2);
  });
  await check('more than fifty related sides paginate without an active cap', async () => {
    while (ids.length < 53) ids.push((await create(parent)).meta.id);
    const all = await list(parent); assert.equal(all.length, 53); assert.equal(new Set(all.map(meta => meta.id)).size, 53);
    const rows = (await call('WatchChats')).filter(chat => chat.sideChatParentId === parent); assert.equal(rows.length, 53);
    save('paginated-list', all);
  });
} catch (error) { failure = error.stack || String(error); }
finally {
  try { await stop(); } catch (error) { failure ||= String(error); }
  await new Promise(resolve => log.end(resolve));
  creationMs.sort((a, b) => a - b);
  save('summary', { passed: !failure, checks, failure, provider: 'mock', parent, sideIds: ids,
    binary, binarySha256: crypto.createHash('sha256').update(fs.readFileSync(binary)).digest('hex'),
    creation: { count: creationMs.length, p50Ms: creationMs[Math.floor(creationMs.length * .5)], p95Ms: creationMs[Math.floor(creationMs.length * .95)], maxMs: creationMs.at(-1) },
    gitHead: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim() });
  const manifest = fs.readdirSync(evidence).filter(name => /\.(json|log)$/.test(name)).map(name => ({ path: name,
    sha256: crypto.createHash('sha256').update(fs.readFileSync(path.join(evidence, name))).digest('hex') }));
  save('manifest', manifest);
  console.log(`Evidence: ${evidence}`);
}
if (failure) { console.error(failure); process.exitCode = 1; }
