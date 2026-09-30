#!/usr/bin/env node
// Coordinator-only: built LUNA engine -> adapter -> deterministic native peer -> durable document.
// Never creates worktrees. Fixture evidence is not real-provider or native UI proof.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const binary = process.env.LUNA_REVIEW_QA_BINARY || path.join(root, 'target/debug/luna');
const base = process.env.LUNA_REVIEW_QA_EVIDENCE || path.join(root, 'target/qa/native-review');
fs.mkdirSync(base, { recursive: true });
const evidence = fs.mkdtempSync(path.join(base, 'run-'));
const project = path.join(evidence, 'project');
const data = path.join(evidence, 'data');
const home = path.join(evidence, 'codex-home');
for (const folder of [project, data, home]) fs.mkdirSync(folder);
execFileSync('git', ['init', '-b', 'main', project]);
fs.writeFileSync(path.join(project, 'fixture.js'), 'export function value() { return 1; }\n');
execFileSync('git', ['-C', project, 'add', 'fixture.js']);
execFileSync('git', ['-C', project, '-c', 'user.name=LUNA QA', '-c', 'user.email=qa@example.invalid', 'commit', '-m', 'fixture']);
const sha = execFileSync('git', ['-C', project, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
fs.appendFileSync(path.join(project, 'fixture.js'), 'export const change = true;\n');
const log = fs.createWriteStream(path.join(evidence, 'engine.log'));
const checks = [];
let child, ws, nextId = 0, failure;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const port = await new Promise(resolve => {
  const server = net.createServer();
  server.listen(0, '127.0.0.1', () => { const number = server.address().port; server.close(() => resolve(number)); });
});
async function start() {
  const env = { ...process.env };
  for (const key of ['LUNA_EDGE_TOKEN', 'LUNA_WORKOS_CLIENT_ID', 'LUNA_ORG_ID']) delete env[key];
  child = spawn(binary, ['headless'], { cwd: root, env: { ...env,
    LUNA_DATA_DIR: data, LUNA_IPC_PORT: String(port), LUNA_EDGE_URL: '', LUNA_HARNESS: 'codex',
    CODEX_HOME: home, CODEX_EXECUTABLE: path.join(root, 'scripts/fixtures/native-review-codex.py'),
  }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout.pipe(log, { end: false }); child.stderr.pipe(log, { end: false });
  for (let i = 0; i < 100; i++) {
    assert.equal(child.exitCode, null, 'engine exited before IPC was ready');
    try {
      ws = new WebSocket(`ws://127.0.0.1:${port}`);
      await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
      return;
    } catch { ws?.close(); await delay(100); }
  }
  throw new Error('IPC startup timed out');
}
async function stop() {
  ws?.close();
  if (child && child.exitCode === null) {
    child.kill('SIGTERM');
    await new Promise((resolve, reject) => { const timeout = setTimeout(() => reject(new Error('engine did not stop')), 10000); child.once('exit', () => { clearTimeout(timeout); resolve(); }); });
  }
}
function call(method, params = {}) {
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 15000);
    function receive(event) {
      const frame = JSON.parse(event.data); if (frame.id !== id) return;
      clearTimeout(timeout); ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(JSON.stringify(frame.err)));
      else { if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true })); resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok); }
    }
    ws.addEventListener('message', receive); ws.send(JSON.stringify({ id, method, params }));
  });
}
async function until(fn, label) {
  for (let i = 0; i < 160; i++) { const value = await fn(); if (value) return value; await delay(100); }
  throw new Error(`Timed out: ${label}`);
}
const config = { harness: 'codex', model: null, reasoning: null, modelOptions: {}, sandbox: 'workspace-write', permissionMode: 'ask' };
const projectId = crypto.randomUUID(), sourceId = crypto.randomUUID(), separateId = crypto.randomUUID();
async function mutate(params) {
  const result = await call('Mutate', params);
  const receipt = result?.receipt;
  if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
}
async function run(taskId, target) {
  const snapshot = await call('GetTaskCommandSnapshot', { taskId });
  const commandId = crypto.randomUUID();
  const s = snapshot.snapshot;
  const request = { prompt: `Native review: ${target.type}`, review: { target }, harness: 'codex',
    providerAccountId: null, model: null, reasoning: null, modelOptions: {}, cwd: project,
    sandbox: 'workspace-write', permissionMode: 'ask', autoApprove: false, resume: null, attachments: [], worktree: null };
  const receipt = await call('ExecuteTaskCommand', { commandId, taskId, actorId: 'review-qa', clientId: 'review-qa', clientSessionId: 'review-qa',
    expectedTaskRevision: s.taskRevision, expectedBindingId: s.bindingId ?? null, expectedBindingGeneration: s.bindingGeneration ?? null,
    intent: { kind: 'session', command: { kind: 'run', request, messageId: commandId } } });
  return { receipt, commandId };
}
async function messages(taskId) { return call('WatchDocMessages', { chatId: taskId }); }
function capture() { const file = path.join(project, '.native-review-protocol.jsonl'); return fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : []; }
async function check(name, fn) { await fn(); checks.push(name); console.log(`PASS ${name}`); }
try {
  await start();
  const info = await call('EngineInfo');
  await mutate({ op: 'createSpace', spaceId: projectId, deviceId: info.deviceId, path: project, name: 'Native review QA' });
  await mutate({ op: 'createChat', chatId: sourceId, spaceId: projectId, cwd: project, config });
  for (const target of [{ type: 'uncommittedChanges' }, { type: 'baseBranch', branch: 'main' }, { type: 'commit', sha, title: 'fixture' }, { type: 'custom', instructions: 'Inspect fixture.js' }]) {
    await check(`native ${target.type}, lifecycle and exactly one durable finding`, async () => {
      await run(sourceId, target);
      await until(async () => JSON.stringify(await messages(sourceId)).includes(`Fixture finding for ${target.type}`), target.type);
      const text = JSON.stringify(await messages(sourceId));
      assert.equal(text.split(`Fixture finding for ${target.type}`).length - 1, 1);
      const calls = capture().filter(item => item.method === 'review/start');
      assert.deepEqual(calls.at(-1).params.target, target);
      assert.equal(calls.at(-1).params.delivery, 'inline');
      assert.ok(text.includes('Review') || text.includes('review'));
      await until(async () => (await call('WatchSessions')).find(item => item.chatId === sourceId)?.status !== 'working', 'review turn settled');
    });
  }
  await check('separate ordinary LUNA chat keeps source binding', async () => {
    const before = (await call('WatchChats')).find(chat => chat.id === sourceId);
    await mutate({ op: 'createChat', chatId: separateId, spaceId: projectId, cwd: project, config });
    await run(separateId, { type: 'custom', instructions: 'Separate review' });
    await until(async () => JSON.stringify(await messages(separateId)).includes('Fixture finding'), 'separate findings');
    const chats = await call('WatchChats');
    assert.equal(chats.find(chat => chat.id === sourceId).harnessSessionId, before.harnessSessionId);
    assert.notEqual(chats.find(chat => chat.id === separateId).harnessSessionId, before.harnessSessionId);
  });
  await check('invalid empty target is rejected before native dispatch', async () => {
    const count = capture().filter(item => item.method === 'review/start').length;
    const sent = await run(sourceId, { type: 'custom', instructions: '  ' });
    await until(async () => { const receipt = await call('GetTaskCommandReceipt', { commandId: sent.commandId }); return ['failed', 'rejected'].includes(receipt.status); }, 'invalid review rejected');
    assert.equal(capture().filter(item => item.method === 'review/start').length, count);
  });
  await check('native provider rejection is visible', async () => {
    await run(sourceId, { type: 'custom', instructions: '[reject]' });
    await until(async () => JSON.stringify(await messages(sourceId)).includes('review fixture rejects'), 'provider error');
    await until(async () => !(await call('WatchSessions')).some(item => item.chatId === sourceId && item.status === 'working'), 'provider rejection settled');
  });
  await check('missing native reviewer output is a visible failure', async () => {
    await run(sourceId, { type: 'custom', instructions: '[missing]' });
    await until(async () => JSON.stringify(await messages(sourceId)).includes('Native review ended without reviewer output'), 'missing output failure');
    await until(async () => !(await call('WatchSessions')).some(item => item.chatId === sourceId && item.status === 'working'), 'missing output settled');
  });
  await check('active review rejects another review and Stop reaches native turn', async () => {
    await run(sourceId, { type: 'custom', instructions: '[hold]' });
    await until(async () => capture().filter(item => item.method === 'review/start').at(-1)?.params.target.instructions === '[hold]' && (await call('WatchSessions')).some(item => item.chatId === sourceId && item.status === 'working'), 'held review working');
    const count = capture().filter(item => item.method === 'review/start').length;
    const competing = await run(sourceId, { type: 'uncommittedChanges' });
    assert.ok(['rejected', 'conflict', 'failed'].includes(competing.receipt.status), JSON.stringify(competing.receipt));
    assert.equal(capture().filter(item => item.method === 'review/start').length, count);
    const { snapshot } = await call('GetTaskCommandSnapshot', { taskId: sourceId });
    await call('ExecuteTaskCommand', { commandId: crypto.randomUUID(), taskId: sourceId,
      actorId: 'review-qa', clientId: 'review-qa', clientSessionId: 'review-qa',
      expectedBindingGeneration: snapshot.bindingGeneration, expectedTurnId: snapshot.activeTurnId,
      intent: { kind: 'session', command: { kind: 'interrupt' } } });
    await until(async () => capture().some(item => item.method === 'turn/interrupt'), 'native interrupt');
    await until(async () => !(await call('WatchSessions')).some(item => item.chatId === sourceId && item.status === 'working'), 'interrupted review settled');
  });
  await check('unexpected review thread fails without rebinding', async () => {
    const before = (await call('WatchChats')).find(chat => chat.id === sourceId).harnessSessionId;
    await run(sourceId, { type: 'custom', instructions: '[foreign]' });
    await until(async () => JSON.stringify(await messages(sourceId)).includes('unexpected review thread'), 'thread identity failure');
    assert.equal((await call('WatchChats')).find(chat => chat.id === sourceId).harnessSessionId, before);
  });
  await check('findings survive engine restart', async () => {
    await stop(); await start();
    const text = JSON.stringify(await messages(sourceId));
    for (const type of ['uncommittedChanges', 'baseBranch', 'commit', 'custom']) assert.equal(text.split(`Fixture finding for ${type}`).length - 1, 1);
  });
  assert.equal(capture().filter(item => item.method === 'turn/start').length, 0, 'no ordinary turn substitute');
} catch (error) { failure = String(error.stack || error); console.error(failure); process.exitCode = 1; }
finally {
  try { await stop(); } catch (error) { failure = `${failure || ''}\n${error}`; process.exitCode = 1; }
  await new Promise(resolve => log.end(resolve));
  const files = ['engine.log', 'project/.native-review-protocol.jsonl'].filter(file => fs.existsSync(path.join(evidence, file)));
  const hashes = Object.fromEntries(files.map(file => [file, crypto.createHash('sha256').update(fs.readFileSync(path.join(evidence, file))).digest('hex')]));
  fs.writeFileSync(path.join(evidence, 'result.json'), JSON.stringify({ proof: 'deterministic adapter/engine/document; no live provider or UI proof', checks, failure, sourceId, separateId, binary, hashes }, null, 2));
  console.log(`Evidence: ${evidence}`);
}
