#!/usr/bin/env node
// CORE-00 current headless engine benchmark. Native rendering is separately
// owned by macos-resource-profile; RPC latency is never called paint latency.
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync, readFileSync, openSync, closeSync, copyFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { createServer } from 'node:net';
import assert from 'node:assert/strict';
import { setTimeout as sleep } from 'node:timers/promises';

const [binaryArg, outputArg] = process.argv.slice(2);
assert.ok(binaryArg && outputArg, 'usage: qa-core-baseline.mjs BINARY OUTPUT_DIR');
const output = resolve(outputArg), source = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
mkdirSync(output, { recursive: false });
const binary = `${output}/luna-measured`;
copyFileSync(resolve(binaryArg), binary);
// A bundle-signed Mach-O copied out of its bundle needs its own ad-hoc seal.
// Retain both hashes; never modify the supplied bundle or the shared target.
execFileSync('codesign', ['--force', '--sign', '-', binary], { stdio: 'pipe' });
execFileSync('codesign', ['--verify', '--strict', binary], { stdio: 'pipe' });
const hash = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const save = (name, value) => writeFileSync(`${output}/${name}.json`, JSON.stringify(value, null, 2));
const probe = `${output}/resource-stat`;
execFileSync('xcrun', ['clang', '-O2', '-Wall', '-Wextra', 'scripts/macos-resource-stat.c', '-o', probe]);
const listener = createServer();
await new Promise(r => listener.listen(0, '127.0.0.1', r));
const port = listener.address().port;
await new Promise(r => listener.close(r));
const env = { ...process.env, LUNA_DATA_DIR: `${output}/data`, LUNA_IPC_PORT: String(port),
  LUNA_EDGE_URL: 'luna-local://offline', LUNA_HARNESS: 'mock', LUNA_QA_MCP_ISOLATION: '1',
  LUNA_WORKTREES_DIR: `${output}/worktrees`, RUST_LOG: 'warn', LUNA_MOCK_DELAY_MS: '20', LUNA_MOCK_CHARS: '24' };
delete env.LUNA_EDGE_TOKEN;
delete env.LUNA_ORG_ID;
let child, closed, ws, seq = 0, phase = 'startup', sampler;
const pending = new Map(), samples = [], startup = [], requests = [], receipts = [], shutdowns = [];
function onMessage(event) {
  for (const line of String(event.data).split('\n').filter(Boolean)) {
    const frame = JSON.parse(line), request = pending.get(frame.id);
    if (!request) continue;
    if ('err' in frame) { pending.delete(frame.id); request.reject(Error(JSON.stringify(frame.err))); }
    else if ('ok' in frame || 'item' in frame) {
      pending.delete(frame.id);
      if ('item' in frame) ws.send(JSON.stringify({ id: frame.id, cancel: true }));
      request.resolve('item' in frame ? frame.item : frame.ok);
    }
  }
}
function call(method, params = {}) {
  const start = performance.now();
  return new Promise((resolve, reject) => {
    const id = ++seq;
    const timer = setTimeout(() => { pending.delete(id); reject(Error(`${method}: timeout; not replayed`)); }, 30000);
    pending.set(id, {
      resolve: result => { clearTimeout(timer); requests.push({ phase, method, ms: performance.now() - start }); resolve(result); },
      reject: error => { clearTimeout(timer); reject(error); },
    });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function start(index) {
  const log = openSync(`${output}/engine-${index}.log`, 'w');
  const began = performance.now();
  child = spawn(binary, ['headless'], { env, detached: true, stdio: ['ignore', log, log] });
  closeSync(log);
  closed = once(child, 'exit');
  for (let i = 0; i < 200; i++) {
    assert.equal(child.exitCode, null, 'engine exited before readiness');
    assert.equal(child.signalCode, null, 'engine killed before readiness');
    try {
      const candidate = new WebSocket(`ws://127.0.0.1:${port}`);
      await new Promise((yes, no) => { candidate.onopen = yes; candidate.onerror = no; });
      ws = candidate; break;
    } catch { await sleep(25); }
  }
  assert.ok(ws && ws.readyState === WebSocket.OPEN, 'engine did not listen');
  ws.onmessage = onMessage;
  await call('EngineReady');
  startup.push({ index, ms: performance.now() - began, data: index ? 'existing profile' : 'fresh profile', osCache: 'uncontrolled; no purge', pid: child.pid });
}
async function stop() {
  if (child && ws?.readyState === WebSocket.OPEN && child.exitCode === null && child.signalCode === null) {
    try { await call('StopEngine'); } catch { /* Scoped process-group cleanup below. */ }
  }
  ws?.close(); ws = null;
  if (closed) {
    const pid = child.pid;
    const killGroup = signal => { try { process.kill(-pid, signal); } catch { /* Group already gone. */ } };
    const timeout = setTimeout(() => killGroup('SIGTERM'), 5000);
    const escalation = setTimeout(() => killGroup('SIGKILL'), 6000);
    await closed;
    clearTimeout(timeout);
    clearTimeout(escalation);
    const remaining = () => execFileSync('ps', ['-axo', 'pid=,pgid='], { encoding: 'utf8' }).trim().split('\n').map(line => line.trim().split(/\s+/).map(Number)).filter(row => row[1] === pid).map(row => row[0]);
    const before = remaining();
    if (before.length) { killGroup('SIGTERM'); await sleep(250); }
    if (remaining().length) { killGroup('SIGKILL'); await sleep(250); }
    const after = remaining();
    shutdowns.push({ pid, exitCode: child.exitCode, signal: child.signalCode, descendantsAfterEngineExit: before, descendantsAfterCleanup: after });
    assert.equal(after.length, 0, 'owned benchmark process group survived cleanup');
  }
  child = null; closed = null;
}
function sample() {
  if (!child) return;
  const rows = execFileSync('ps', ['-axo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).trim().split('\n').map(line => {
    const [, pid, parent, executable] = line.match(/^\s*(\d+)\s+(\d+)\s+(.+)$/);
    return { pid: Number(pid), parent: Number(parent), executable };
  });
  const descendants = parent => rows.filter(row => row.parent === parent).flatMap(({pid}) => [pid, ...descendants(pid)]);
  const pids = [child.pid, ...descendants(child.pid)];
  const processes = JSON.parse(execFileSync(probe, pids.map(String), { encoding: 'utf8' })).map(stat => ({ ...stat, executable: rows.find(row => row.pid === stat.pid)?.executable }));
  samples.push({ at: Date.now(), phase, processes, aggregateFootprintMiB: processes.reduce((sum, p) => sum + p.footprintMiB, 0) });
}
async function command(taskId, intent) {
  const { snapshot } = await call('GetTaskCommandSnapshot', { taskId });
  const result = await call('ExecuteTaskCommand', { taskId, commandId: randomUUID(), actorId: 'core-00-benchmark', clientId: 'core-00', clientSessionId: 'core-00',
    expectedTaskRevision: snapshot.taskRevision, expectedBindingGeneration: snapshot.bindingGeneration, intent });
  receipts.push(result);
  assert.ok(intent.kind === 'createChat' ? result.status === 'applied'
    : ['applied', 'accepted'].includes(result.status) || (result.status === 'pending' && result.reason?.code === 'providerAcceptanceUnknown'), JSON.stringify(result));
  return result;
}
async function transcript(taskId) {
  const value = await call('WatchDocMessages', { chatId: taskId });
  const entries = value.frame?.reset ?? value.reset;
  assert.ok(Array.isArray(entries), 'expected real transcript reset');
  return entries;
}
async function run(taskId, prompt) {
  const before = await transcript(taskId);
  const id = randomUUID();
  await command(taskId, { kind: 'session', command: { kind: 'run', messageId: id, request: {
    prompt, model: null, reasoning: null, modelOptions: {}, cwd: project, sandbox: 'workspace-write', autoApprove: true, resume: null,
  } } });
  for (let i = 0; i < 300; i++) {
    await sleep(100);
    const entries = await transcript(taskId);
    const last = entries.at(-1);
    if (entries.length > before.length && last?.role === 'assistant' && last.status === 'complete') {
      assert.ok(entries.some(entry => entry.role === 'user' && entry.parts.some(part => part.text === prompt)));
      return entries;
    }
  }
  throw Error(`turn incomplete: ${taskId}`);
}
let project;
try {
  // First fresh-profile launch then repeated process launches; never called OS-cold.
  for (let i = 0; i < 10; i++) { await start(i); if (i < 9) await stop(); }
  sampler = setInterval(sample, 500);
  phase = 'idle'; await sleep(5000);
  project = `${output}/small-project`;
  mkdirSync(project); execFileSync('git', ['init', '-q', project]);
  writeFileSync(`${project}/README.md`, 'CORE-00 isolated workload\n');
  const { deviceId } = await call('EngineInfo'), spaceId = randomUUID();
  await call('Mutate', { op: 'createSpace', spaceId, deviceId, path: project });
  const tasks = [];
  phase = 'create-chats';
  for (let i = 0; i < 40; i++) {
    const task = randomUUID(); tasks.push(task);
    await command(task, { kind: 'createChat', spaceId, cwd: project, title: `CORE-00 chat ${i}`, config: { harness: 'mock', sandbox: 'workspace-write' } });
  }
  phase = 'multiple-working-chats';
  const concurrent = Promise.all(tasks.slice(0, 5).map((task, i) => run(task, `Concurrent controlled workload ${i}`)));
  // Attach a rejection handler immediately while reading independent session state.
  concurrent.catch(() => {});
  let liveSessions = [];
  for (let i = 0; i < 200; i++) {
    liveSessions = await call('WatchSessions');
    if (liveSessions.filter(session => tasks.slice(0, 5).includes(session.chatId) && session.status === 'working').length >= 2) break;
    await sleep(50);
  }
  save('concurrent-sessions', liveSessions);
  assert.ok(liveSessions.filter(session => tasks.slice(0, 5).includes(session.chatId) && session.status === 'working').length >= 2, 'no actual overlapping runs');
  await concurrent;
  phase = 'long-transcript';
  let entries;
  for (let i = 0; i < 20; i++) entries = await run(tasks[0], `Turn ${i}: ` + 'A reproducible long transcript paragraph with code and ordinary prose.\n'.repeat(100));
  save('transcript', entries);
  save('frames', [{ at: 0, frame: { reset: entries } }]);
  // Register the actual large source tree read-only; no provider executes commands.
  phase = 'large-repository'; project = process.cwd();
  const largeSpace = randomUUID(), largeTask = randomUUID();
  await call('Mutate', { op: 'createSpace', spaceId: largeSpace, deviceId, path: project });
  await command(largeTask, { kind: 'createChat', spaceId: largeSpace, cwd: project, title: 'CORE-00 large source tree', config: { harness: 'mock', sandbox: 'workspace-write' } });
  await run(largeTask, 'Controlled run in the large repository; mock executes no shell tools.');
  phase = 'settled'; await sleep(10000); sample();
  clearInterval(sampler);
  await stop(); await start(10);
  const restored = await transcript(tasks[0]);
  assert.deepEqual(restored, entries, 'restart changed the measured transcript');
  await stop();
  save('result', { passed: true, source, sourceBinarySha256: hash(resolve(binaryArg)), binarySha256: hash(binary), boundary: 'real headless engine + RPC + durable stores, controlled mock provider',
    startup, requests, receipts, samples, shutdowns, chatCount: 41, requestedConcurrentChats: 5,
    observedConcurrentChats: liveSessions.filter(session => tasks.slice(0, 5).includes(session.chatId) && session.status === 'working').length,
    transcriptEntries: entries.length,
    transcriptBytes: Buffer.byteLength(JSON.stringify(entries)), restored: true,
    largeRepositoryTrackedFiles: execFileSync('git', ['ls-files', '-z']).toString().split('\0').filter(Boolean).length,
    limitations: ['No live provider or network throughput', 'No UI/GPU or event-to-paint metric', 'Large repository registration/run, not a Git diff or search benchmark'] });
  console.log(JSON.stringify({ passed: true, output, transcriptEntries: entries.length }));
} catch (error) {
  save('failure', { error: error.stack, startup, requests, receipts, samples });
  throw error;
} finally { clearInterval(sampler); await stop(); }
