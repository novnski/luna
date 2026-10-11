#!/usr/bin/env node
// CORE-01 wire capture: a disposable headless engine (own data dir and port)
// runs one fixed, provider-free RPC session and every raw request and reply
// frame is recorded. Run it on the build before and after a contract change and
// compare: existing replies and receipts must not change (C5-C7, C9).
//
//   node scripts/qa-contract-wire.mjs /absolute/out-dir [path/to/luna]
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import {spawn} from 'node:child_process';

const out = process.argv[2];
assert.ok(out && path.isAbsolute(out), 'usage: qa-contract-wire.mjs /absolute/out-dir [luna-binary]');
const root = path.resolve(import.meta.dirname, '..');
const binary = process.argv[3] ?? path.join(root, 'target/debug/luna');
fs.mkdirSync(out, {recursive: true});
const data = fs.mkdtempSync(path.join(out, 'data-'));
// A fixed project path keeps create-chat's payload (and hash) comparable
// between captures; the intent's cwd is part of the hashed payload.
const project = path.join(os.tmpdir(), 'luna-core01-wire-project');
fs.rmSync(project, {recursive: true, force: true});
fs.mkdirSync(project, {recursive: true});
const port = await new Promise(resolve => {
  const server = net.createServer();
  server.listen(0, '127.0.0.1', () => { const p = server.address().port; server.close(() => resolve(p)); });
});
const env = {...process.env, LUNA_DATA_DIR: data, LUNA_IPC_PORT: String(port), LUNA_EDGE_URL: ''};
for (const name of ['LUNA_EDGE_TOKEN', 'LUNA_WORKOS_CLIENT_ID', 'LUNA_ORG_ID']) delete env[name];
const engine = spawn(binary, ['headless'], {cwd: root, env, stdio: ['ignore', 'pipe', 'pipe']});
const log = fs.createWriteStream(path.join(out, 'engine.log'));
engine.stdout.pipe(log, {end: false}); engine.stderr.pipe(log, {end: false});
let ws;
for (let i = 0; i < 150 && !ws; i++) {
  assert.equal(engine.exitCode, null, 'engine exited during startup');
  try {
    const socket = new WebSocket(`ws://127.0.0.1:${port}`);
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, {once: true}); socket.addEventListener('error', reject, {once: true});
    });
    ws = socket;
  } catch { await new Promise(resolve => setTimeout(resolve, 100)); }
}
assert.ok(ws, 'IPC startup timed out');

const frames = [];
let nextId = 0;
// Sends `raw` text exactly (key order matters for C7) and records the first
// reply frame that settles it.
function send(label, raw) {
  const id = ++nextId;
  const text = raw.replace('"@ID@"', String(id));
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} timed out`)), 30000);
    function receive(event) {
      for (const line of String(event.data).split('\n').filter(Boolean)) {
        const frame = JSON.parse(line);
        if (frame.id !== id || !['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) continue;
        clearTimeout(timer); ws.removeEventListener('message', receive);
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({id, cancel: true}));
        frames.push({label, request: JSON.parse(text), requestText: text, reply: frame});
        resolve(frame);
      }
    }
    ws.addEventListener('message', receive);
    ws.send(text);
  });
}
const call = (label, method, params) => send(label, JSON.stringify({id: '@ID@', method, params}));

const taskId = '6f1d2c3b-4a59-4e68-9b7a-0c1d2e3f4a5b';
const spaceId = '0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d';
const actor = {actorId: 'core01-actor', clientId: 'core01-client', clientSessionId: 'core01-session'};
try {
  const info = (await call('engine-info', 'EngineInfo', {})).ok;
  await call('list-harnesses', 'ListHarnesses', {});
  await call('watch-chats', 'WatchChats', {});
  await call('create-space', 'Mutate', {op: 'createSpace', spaceId, deviceId: info.deviceId, path: project, name: 'CORE-01 wire', gitDetected: false});
  const config = {harness: 'codex', providerAccountId: null, model: null, reasoning: null, modelOptions: {},
    sandbox: 'read-only', permissionMode: 'ask', effectivePermissionMode: 'ask'};
  await call('create-chat', 'ExecuteTaskCommand', {commandId: 'core01-create', ...actor, taskId, issuedAtMs: 1791700000000,
    intent: {kind: 'createChat', spaceId, cwd: project, config}});
  await call('rename-without-revision', 'ExecuteTaskCommand', {commandId: 'core01-unfenced', ...actor, taskId, issuedAtMs: 1791700000001,
    intent: {kind: 'rename', title: 'Unfenced'}});
  await call('rename', 'ExecuteTaskCommand', {commandId: 'core01-rename', ...actor, taskId, issuedAtMs: 1791700000002,
    expectedTaskRevision: 1, intent: {kind: 'rename', title: 'Wire capture'}});
  // C7: the identical command with every key in a different order.
  await send('rename-reordered-retry', `{"params":{"intent":{"title":"Wire capture","kind":"rename"},"expectedTaskRevision":1,"issuedAtMs":1791700000002,"taskId":"${taskId}","clientSessionId":"core01-session","clientId":"core01-client","actorId":"core01-actor","commandId":"core01-rename"},"method":"ExecuteTaskCommand","id":"@ID@"}`);
  await call('rename-id-conflict', 'ExecuteTaskCommand', {commandId: 'core01-rename', ...actor, taskId, issuedAtMs: 1791700000003,
    expectedTaskRevision: 1, intent: {kind: 'rename', title: 'A different title'}});
  await call('rename-stale-revision', 'ExecuteTaskCommand', {commandId: 'core01-stale', ...actor, taskId, issuedAtMs: 1791700000004,
    expectedTaskRevision: 0, intent: {kind: 'rename', title: 'Stale'}});
  await call('pin', 'ExecuteTaskCommand', {commandId: 'core01-pin', ...actor, taskId, issuedAtMs: 1791700000005,
    expectedTaskRevision: 2, intent: {kind: 'setPinned', pinned: true}});
  await call('receipt-read', 'GetTaskCommandReceipt', {commandId: 'core01-rename'});
  await call('snapshot-read', 'GetTaskCommandSnapshot', {taskId});
  await call('unknown-method', 'FutureDangerousMethod', {});
  await call('bad-params', 'ExecuteTaskCommand', {taskId});
} finally {
  fs.writeFileSync(path.join(out, 'frames.json'), `${JSON.stringify(frames, null, 1)}\n`);
  ws.close();
  engine.kill('SIGTERM');
  await new Promise(resolve => engine.once('exit', resolve));
}
console.log(JSON.stringify({frames: frames.length, out}));
