#!/usr/bin/env node
// Actual isolated engine/document/queue E2E with the synthetic Mock adapter.
// Failure expectations were recorded in docs/qa/child-chats.md before this harness.
// Does not build, create worktrees, use user data, or claim real-provider/native proof.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
const root = path.resolve(import.meta.dirname, '..');
const binary = process.env.LUNA_CHILD_QA_BINARY || path.join(root, 'target/debug/luna');
const outputRoot = process.env.LUNA_CHILD_QA_EVIDENCE;
assert.ok(outputRoot && path.isAbsolute(outputRoot), 'Set LUNA_CHILD_QA_EVIDENCE to an absolute preserved evidence directory');
fs.mkdirSync(outputRoot, { recursive: true });
const evidence = fs.mkdtempSync(path.join(outputRoot, 'engine-'));
const data = path.join(evidence, 'data'); const project = path.join(evidence, 'project');
fs.mkdirSync(data); fs.mkdirSync(project);
const log = fs.createWriteStream(path.join(evidence, 'engine.log'));
let engine; let ws; let nextId = 0; const checks = []; const saved = {};
const save = (name, value) => { saved[name] = value; fs.writeFileSync(path.join(evidence, `${name}.json`), JSON.stringify(value, null, 2) + '\n'); };
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const port = await new Promise(resolve => { const server = net.createServer(); server.listen(0, '127.0.0.1', () => { const p = server.address().port; server.close(() => resolve(p)); }); });
async function start() {
  const env = { ...process.env, LUNA_DATA_DIR: data, LUNA_IPC_PORT: String(port), LUNA_HARNESS: 'mock', LUNA_EDGE_URL: '' };
  for (const name of ['LUNA_EDGE_TOKEN', 'LUNA_WORKOS_CLIENT_ID', 'LUNA_ORG_ID']) delete env[name];
  engine = spawn(binary, ['headless'], { cwd: root, env, stdio: ['ignore', 'pipe', 'pipe'] });
  engine.stdout.pipe(log, { end: false }); engine.stderr.pipe(log, { end: false });
  for (let i = 0; i < 100; i++) {
    assert.equal(engine.exitCode, null, 'isolated engine exited');
    try { ws = new WebSocket(`ws://127.0.0.1:${port}`); await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); }); return; }
    catch { ws?.close(); await new Promise(resolve => setTimeout(resolve, 100)); }
  }
  throw new Error('IPC startup timeout');
}
async function stop() {
  ws?.close(); ws = undefined;
  if (!engine || engine.exitCode !== null) return;
  const own = engine; own.kill('SIGTERM');
  await new Promise((resolve, reject) => { const timer = setTimeout(() => reject(new Error('isolated engine did not exit gracefully')), 10000); own.once('exit', () => { clearTimeout(timer); resolve(); }); });
  engine = undefined;
}
function call(method, params = {}) {
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timeout`)); }, 15000);
    function receive(event) {
      for (const raw of String(event.data).split('\n').filter(Boolean)) {
        const frame = JSON.parse(raw); if (frame.id !== id) continue;
        if (!Object.hasOwn(frame, 'err') && !Object.hasOwn(frame, 'ok') && !Object.hasOwn(frame, 'item')) continue;
        clearTimeout(timer); ws.removeEventListener('message', receive);
        if (frame.err) reject(new Error(JSON.stringify(frame.err)));
        else { if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true })); resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok); }
      }
    }
    ws.addEventListener('message', receive); ws.send(JSON.stringify({ id, method, params }));
  });
}
async function check(name, fn) { await fn(); checks.push(name); console.log(`PASS ${name}`); }
async function until(fn, timeout = 45000) { const end = Date.now() + timeout; while (Date.now() < end) { const value = await fn(); if (value) return value; await new Promise(resolve => setTimeout(resolve, 200)); } throw new Error('readback timeout'); }
const parent = crypto.randomUUID(); const space = crypto.randomUUID();
const request = { prompt: 'Only reply CHILD_CHAT_QA_OK. Do not create additional chats.', title: 'Persistent child QA', idempotencyKey: 'first-child', reportResults: true, continueParent: false };
const config = { harness: 'mock', model: 'mock-model', sandbox: 'workspace-write', permissionMode: 'auto', effectivePermissionMode: 'auto', modelOptions: {} };
let failure;
try {
  await start(); const info = await call('EngineInfo'); save('engine-info', info);
  await call('Mutate', { op: 'createSpace', spaceId: space, deviceId: info.deviceId, path: project });
  await call('Mutate', { op: 'createChat', chatId: parent, spaceId: space, cwd: project, config });
  const receipt = await call('CreateChildThread', { parentTaskId: parent, request }); save('creation-receipt', receipt);
  const child = receipt.taskId;
  await check('same request yields the same child and creation receipt', async () => {
    const retry = await call('CreateChildThread', { parentTaskId: parent, request }); save('duplicate-receipt', retry);
    assert.equal(retry.taskId, child); assert.equal(retry.command.commandId, receipt.command.commandId);
    assert.equal((await call('WatchChats')).filter(chat => chat.childThread?.parentTaskId === parent).length, 1);
  });
  await check('same identity with changed prompt conflicts', async () => {
    await assert.rejects(call('CreateChildThread', { parentTaskId: parent, request: { ...request, prompt: 'different payload' } }));
  });
  await check('lineage is distinct from fork/lifecycle ownership', async () => {
    const identity = await call('GetTaskIdentity', { taskId: child }); save('identity', identity);
    assert.equal(identity.task.lineage.parentTaskId, parent);
    assert.ok(!identity.task.lineage.sourceTaskId); assert.ok(!identity.task.lineage.lifecycleOwnerTaskId);
  });
  await check('startup enters the durable ordinary queue', async () => {
    const startup = await until(async () => await call('GetTaskCommandReceipt', { commandId: `child-start:${child}` })); save('startup-receipt', startup);
    assert.equal(startup.status, 'applied'); assert.equal(startup.intent, 'queueMessage');
    const row = (await call('WatchChats')).find(chat => chat.id === child); save('child-row', row);
    assert.equal(row.childThread.initialMessageId, receipt.initialMessageId); assert.equal(row.cwd, project);
    assert.equal(row.childThread.permissionCeiling, 'auto');
  });
  await check('outcome outbox persists before deduplicated parent display', async () => {
    const row = await until(async () => (await call('WatchChats')).find(chat => chat.id === child && chat.childThread?.outcomes.some(outcome => outcome.delivered)));
    save('outbox-before-restart', row.childThread.outcomes);
    const transcript = await call('WatchDocMessages', { chatId: parent }); save('parent-transcript-before-restart', transcript);
    const entries = transcript.frame?.reset || transcript.reset;
    assert.ok(entries?.some(entry => entry.id.startsWith('child-outcome:')));
    assert.ok(!entries.some(entry => entry.role === 'user'), 'display-only reporting must not start a parent turn');
  });
  await check('restart retains child, startup receipt and deduplicated outbox', async () => {
    await stop(); await start();
    const retry = await call('CreateChildThread', { parentTaskId: parent, request }); assert.equal(retry.taskId, child);
    const row = (await call('WatchChats')).find(chat => chat.id === child); save('child-row-after-restart', row);
    assert.equal(row.childThread.initialMessageId, receipt.initialMessageId);
    assert.deepEqual(row.childThread.outcomes, saved['outbox-before-restart']);
    const startup = await call('GetTaskCommandReceipt', { commandId: `child-start:${child}` }); assert.equal(startup.commandId, saved['startup-receipt'].commandId);
    const transcript = await call('WatchDocMessages', { chatId: parent }); save('parent-transcript-after-restart', transcript);
    const entries = transcript.frame?.reset || transcript.reset;
    const ids = entries.map(entry => entry.id); assert.equal(ids.length, new Set(ids).size);
  });
  await check('four hierarchy levels allowed; fifth rejected', async () => {
    const grand = await call('CreateChildThread', { parentTaskId: child, request: { ...request, idempotencyKey: 'grand', reportResults: false } });
    const great = await call('CreateChildThread', { parentTaskId: grand.taskId, request: { ...request, idempotencyKey: 'great', reportResults: false } });
    await assert.rejects(call('CreateChildThread', { parentTaskId: great.taskId, request: { ...request, idempotencyKey: 'too-deep' } }));
  });
  await check('archiving a parent preserves independent children', async () => {
    await call('Mutate', { op: 'setChatArchived', chatId: parent, archived: true });
    const rows = await call('WatchChats'); save('archived-parent-readback', rows);
    assert.ok(rows.find(row => row.id === parent).archived); assert.ok(!rows.find(row => row.id === child).archived);
    await assert.rejects(call('CreateChildThread', { parentTaskId: parent, request: { ...request, idempotencyKey: 'archived-parent' } }));
  });
} catch (error) { failure = String(error.stack || error); console.error(failure); process.exitCode = 1; }
finally {
  try { await stop(); } catch (error) { failure ??= String(error); process.exitCode = 1; }
  await new Promise(resolve => log.end(resolve));
  save('result', { status: failure ? 'FAIL' : 'PASS', checks, failure, parent, space, port, binary, binarySha256: hash(binary), sourceCommit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), evidence, proof: 'actual isolated engine with synthetic Mock provider; real Codex tool/approval and native UI acceptance remain separate gates' });
  fs.writeFileSync(path.join(evidence, 'SHA256SUMS'), fs.readdirSync(evidence).filter(name => name.endsWith('.json') || name.endsWith('.log')).map(name => `${hash(path.join(evidence, name))}  ${name}`).join('\n') + '\n');
  console.log(`Evidence: ${evidence}`);
}
