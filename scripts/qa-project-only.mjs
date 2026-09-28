#!/usr/bin/env node
// End-to-end project-only acceptance against an isolated headless LUNA engine.
// Run after `scripts/cargo.sh build --locked -p luna`. Evidence stays under target/qa/.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { spawn } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const binary = path.join(root, 'target/debug/luna');
assert.ok(fs.statSync(binary).isFile(), 'build luna before running this acceptance check');
fs.mkdirSync(path.join(root, 'target/qa'), { recursive: true });
const evidence = fs.mkdtempSync(path.join(root, 'target/qa/project-only-'));
const data = path.join(evidence, 'data');
const project = path.join(evidence, 'project');
fs.mkdirSync(data);
fs.mkdirSync(project);
const log = fs.createWriteStream(path.join(evidence, 'engine.log'), { flags: 'a' });
const checks = [];
let engine;
let ws;
let nextId = 1;

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}
async function start(port) {
  const env = { ...process.env };
  delete env.LUNA_EDGE_TOKEN;
  delete env.LUNA_WORKOS_CLIENT_ID;
  delete env.LUNA_ORG_ID;
  engine = spawn(binary, ['headless'], {
    cwd: root,
    env: {
      ...env,
      LUNA_DATA_DIR: data,
      LUNA_IPC_PORT: String(port),
      LUNA_EDGE_URL: '',
      LUNA_HARNESS: 'mock',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  engine.stdout.pipe(log, { end: false });
  engine.stderr.pipe(log, { end: false });
  for (let attempt = 0; attempt < 100; attempt++) {
    if (engine.exitCode !== null) throw new Error(`headless engine exited ${engine.exitCode}`);
    try {
      ws = new WebSocket(`ws://127.0.0.1:${port}`);
      await new Promise((resolve, reject) => {
        ws.addEventListener('open', resolve, { once: true });
        ws.addEventListener('error', reject, { once: true });
      });
      return;
    } catch {
      ws?.close();
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  }
  throw new Error('headless engine did not open its isolated IPC port');
}
async function stop() {
  ws?.close();
  ws = undefined;
  if (!engine || engine.exitCode !== null) return;
  const child = engine;
  child.kill('SIGTERM');
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('engine did not stop')), 10000);
    child.once('exit', () => { clearTimeout(timeout); resolve(); });
  });
  engine = undefined;
}
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      ws.removeEventListener('message', receive);
      reject(new Error(`${method} timed out`));
    }, 15000);
    function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timeout);
      ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(JSON.stringify(frame.err)));
      else {
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
        resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    }
    ws.addEventListener('message', receive);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function appliedMutation(params) {
  const receipt = await settledMutation(params);
  if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
}
async function settledMutation(params) {
  const result = await call('Mutate', params);
  let receipt = result.receipt;
  for (let attempt = 0; receipt?.status === 'pending' && attempt < 100; attempt++) {
    await new Promise(resolve => setTimeout(resolve, 50));
    receipt = await call('GetTaskCommandReceipt', { commandId: receipt.commandId });
  }
  return receipt;
}
async function check(name, fn) {
  await fn();
  checks.push(name);
  console.log(`PASS ${name}`);
}
function hash(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

const port = await freePort();
const projectId = crypto.randomUUID();
const taskId = crypto.randomUUID();
const refusedId = crypto.randomUUID();
const refusedCommandId = crypto.randomUUID();
let failure;
try {
  await start(port);
  const info = await call('EngineInfo');
  assert.equal(typeof info.deviceId, 'string');
  assert.equal(info.workspaceScope, 'local');
  await check('projectless creation is rejected', async () => {
    const receipt = await settledMutation({
      op: 'createChat', chatId: refusedId, deviceId: info.deviceId,
    });
    assert.equal(receipt?.status, 'failed', JSON.stringify(receipt));
    assert.match(JSON.stringify(receipt), /project is required/i);
    assert.ok(!(await call('WatchChats')).some(chat => chat.id === refusedId));
  });
  await check('projectless task command is rejected', async () => {
    const receipt = await call('ExecuteTaskCommand', {
      commandId: refusedCommandId, taskId: refusedId,
      actorId: info.deviceId, clientId: 'project-only-e2e',
      clientSessionId: crypto.randomUUID(),
      intent: { kind: 'createChat', deviceId: info.deviceId },
    });
    assert.equal(receipt.status, 'failed', JSON.stringify(receipt));
    assert.match(JSON.stringify(receipt), /project is required/i);
    assert.ok(!(await call('WatchChats')).some(chat => chat.id === refusedId));
  });
  await check('project task can be created and listed', async () => {
    await appliedMutation({
      op: 'createSpace', spaceId: projectId, deviceId: info.deviceId,
      path: project, name: 'Project only QA',
    });
    await appliedMutation({ op: 'createChat', chatId: taskId, spaceId: projectId, cwd: project });
    const spaces = await call('WatchSpaces');
    const chats = await call('WatchChats');
    assert.ok(spaces.some(space => space.id === projectId));
    assert.equal(chats.find(chat => chat.id === taskId)?.spaceId, projectId);
    assert.ok(chats.every(chat => chat.spaceId));
  });
  await stop();
  await start(port);
  await check('project task survives restart without projectless rows', async () => {
    const chats = await call('WatchChats');
    assert.equal(chats.find(chat => chat.id === taskId)?.spaceId, projectId);
    assert.ok(chats.every(chat => chat.spaceId));
    assert.ok(!chats.some(chat => chat.id === refusedId));
  });
} catch (error) {
  failure = String(error.stack || error);
  console.error(failure);
  process.exitCode = 1;
} finally {
  try { await stop(); } catch (error) {
    failure ??= String(error.stack || error);
    process.exitCode = 1;
  }
  await new Promise(resolve => log.end(resolve));
  const result = {
    status: failure ? 'FAIL' : 'PASS', checks, failure,
    binarySha256: hash(binary), port, projectId, taskId, refusedId, refusedCommandId,
    evidence, repeatedBy: 'scripts/cargo.sh build --locked -p luna && node scripts/qa-project-only.mjs',
  };
  const resultFile = path.join(evidence, 'result.json');
  fs.writeFileSync(resultFile, `${JSON.stringify(result, null, 2)}\n`);
  fs.writeFileSync(path.join(evidence, 'SHA256SUMS'),
    `${hash(resultFile)}  result.json\n${hash(path.join(evidence, 'engine.log'))}  engine.log\n`);
  console.log(`${result.status} evidence: ${evidence}`);
}
