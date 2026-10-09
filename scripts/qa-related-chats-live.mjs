#!/usr/bin/env node
// Live provider E2E: exact fork context and provider binding across process
// restart. Uses existing provider login through normal LUNA execution paths.
// Failure cases: docs/reference/related-chats-redesign.md (recorded first).
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const binary = process.env.LUNA_RELATED_QA_BINARY || path.join(root, 'target/debug/luna');
const provider = process.env.LUNA_RELATED_QA_PROVIDER || 'codex';
assert.ok(['codex', 'claude-code'].includes(provider));
const model = process.env.LUNA_RELATED_QA_MODEL || (provider === 'codex' ? 'gpt-6.1-sol' : 'claude-sonnet-4-6');
const outputRoot = process.env.LUNA_RELATED_QA_EVIDENCE;
assert.ok(outputRoot && path.isAbsolute(outputRoot), 'Set LUNA_RELATED_QA_EVIDENCE to an absolute evidence directory');
fs.mkdirSync(outputRoot, { recursive: true });
const evidence = fs.mkdtempSync(path.join(outputRoot, `live-${provider}-`));
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
  const env = { ...process.env, LUNA_DATA_DIR: data, LUNA_IPC_PORT: String(port), LUNA_HARNESS: provider, LUNA_EDGE_URL: '' };
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
    const timer = setTimeout(() => { socket.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 60000);
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
  const deadline = Date.now() + 120000;
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

const parent = crypto.randomUUID(), space = crypto.randomUUID();
const codeWord = `RELATED_QA_${crypto.randomUUID().replaceAll('-', '').slice(0, 12)}`;
const config = { harness: provider, model, sandbox: 'workspace-write', permissionMode: 'ask', effectivePermissionMode: 'ask', modelOptions: {} };
const entries = value => value.frame?.reset ?? value.reset ?? [];
const text = entry => (entry.parts || []).filter(part => part.kind === 'text').map(part => part.text).join('\n');
async function transcript(id) { return entries(await call('WatchDocMessages', { chatId: id })); }
async function parentTurn(prompt) {
  const messageId = crypto.randomUUID();
  await call('QueueMessage', { chatId: parent, messageId, text: prompt, holdForTurnEnd: true });
  return until(async () => {
    const rows = await transcript(parent); const at = rows.findIndex(entry => entry.id === messageId);
    const replies = rows.slice(at + 1).filter(entry => entry.role === 'assistant');
    return at >= 0 && replies.some(entry => entry.status === 'complete') && rows;
  });
}
async function sideTurn(id, prompt) {
  const messageId = crypto.randomUUID();
  await call('SendSideChat', { id, messageId, text: prompt });
  return until(async () => {
    const page = await call('ReadSideChat', { id });
    const at = page.entries.findIndex(entry => entry.id === messageId);
    return at >= 0 && !page.working && page.entries.slice(at + 1).some(entry => entry.role === 'assistant' && entry.status === 'complete') && page;
  });
}
let sideId, originalProviderId, creationElapsed;
try {
  fs.writeFileSync(path.join(project, 'README.md'), 'Isolated LUNA related-chat QA. Do not modify files.\n');
  execFileSync('git', ['init', '-q', project]);
  await start(); const info = await call('EngineInfo'); save('engine-info', info);
  assert.ok(info.capabilities.includes('side-chat-persistence-v1'));
  await call('Mutate', { op: 'createSpace', spaceId: space, deviceId: info.deviceId, path: project });
  await call('Mutate', { op: 'createChat', chatId: parent, spaceId: space, cwd: project, config });
  await check('real parent records unique context', async () => {
    const rows = await parentTurn(`This is a brief LUNA application QA. Do not use tools or edit files. Remember this code word in this conversation: ${codeWord}. Reply exactly READY_${codeWord}.`);
    save('parent-before-fork', rows);
    assert.ok(rows.some(entry => entry.role === 'assistant' && text(entry).includes(`READY_${codeWord}`)));
  });
  await check('native fork creates a distinct provider session', async () => {
    const started = performance.now(); const created = await create(parent); creationElapsed = performance.now() - started;
    assert.equal(created.meta.contextMode, 'nativeFork'); sideId = created.meta.id; save('native-fork', created);
    const identity = await call('GetTaskIdentity', { taskId: sideId });
    const parentIdentity = await call('GetTaskIdentity', { taskId: parent });
    originalProviderId = identity.currentBinding.providerTaskId;
    assert.ok(originalProviderId); assert.notEqual(originalProviderId, parentIdentity.currentBinding.providerTaskId);
    save('fork-identity', identity); save('parent-identity', parentIdentity);
  });
  await check('side knows inherited context without receiving the code word again', async () => {
    const page = await sideTurn(sideId, 'Do not use tools. What code word did I tell the parent conversation to remember? Reply with that exact code word only.');
    save('side-inherited-context', page);
    assert.ok(page.entries.some(entry => entry.role === 'assistant' && text(entry).includes(codeWord)));
    assert.ok(!(await transcript(parent)).some(entry => entry.role === 'user' && text(entry).includes('What code word')));
  });
  await stop(); await start();
  await check('process restart resumes the same fork with all context', async () => {
    const identity = await call('GetTaskIdentity', { taskId: sideId });
    assert.equal(identity.currentBinding.providerTaskId, originalProviderId); save('identity-after-restart', identity);
    const before = await call('ReadSideChat', { id: sideId }); assert.equal(before.meta.state, 'active');
    assert.equal(before.entries.filter(entry => entry.role === 'user').length, 1);
    const after = await sideTurn(sideId, 'Do not use tools. Reply with the remembered code word followed by AFTER_RESTART.');
    const last = after.entries.filter(entry => entry.role === 'assistant').at(-1);
    assert.ok(text(last).includes(codeWord) && text(last).includes('AFTER_RESTART'));
    assert.equal(after.entries.filter(entry => entry.role === 'user').length, 2); save('continued-native-fork', after);
    const finalIdentity = await call('GetTaskIdentity', { taskId: sideId });
    assert.equal(finalIdentity.currentBinding.providerTaskId, originalProviderId);
  });
} catch (error) { failure = error.stack || String(error); }
finally {
  try { await stop(); } catch (error) { failure ||= String(error); }
  await new Promise(resolve => log.end(resolve));
  save('summary', { passed: !failure, checks, failure, provider, model, parent, sideId,
    providerSessionId: originalProviderId, creationElapsedMs: creationElapsed, binary,
    binarySha256: crypto.createHash('sha256').update(fs.readFileSync(binary)).digest('hex'),
    sourceCommit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim() });
  save('manifest', fs.readdirSync(evidence).filter(name => /\.(json|log)$/.test(name)).map(name => ({ path: name,
    sha256: crypto.createHash('sha256').update(fs.readFileSync(path.join(evidence, name))).digest('hex') })));
  console.log(`Evidence: ${evidence}`);
}
if (failure) { console.error(failure); process.exitCode = 1; }
