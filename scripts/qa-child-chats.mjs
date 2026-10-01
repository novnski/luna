#!/usr/bin/env node
// Actual isolated engine/document/queue E2E with the synthetic Mock adapter, then
// the Codex adapter's per-process `luna` MCP endpoint driven by a deterministic
// Codex peer (scripts/fixtures/luna-mcp-codex.py) acting as Codex's MCP client.
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
const codexData = path.join(evidence, 'data-codex'); const codexHome = path.join(evidence, 'codex-home');
fs.mkdirSync(data); fs.mkdirSync(project); fs.mkdirSync(codexData); fs.mkdirSync(codexHome);
const log = fs.createWriteStream(path.join(evidence, 'engine.log'));
let engine; let ws; let nextId = 0; const checks = []; const saved = {};
const save = (name, value) => { saved[name] = value; fs.writeFileSync(path.join(evidence, `${name}.json`), JSON.stringify(value, null, 2) + '\n'); };
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const port = await new Promise(resolve => { const server = net.createServer(); server.listen(0, '127.0.0.1', () => { const p = server.address().port; server.close(() => resolve(p)); }); });
async function start(overrides = { LUNA_DATA_DIR: data, LUNA_HARNESS: 'mock' }) {
  const env = { ...process.env, LUNA_IPC_PORT: String(port), LUNA_EDGE_URL: '', ...overrides };
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
    const archived = await call('Mutate', { op: 'setChatArchived', chatId: parent, archived: true }); save('archive-parent-receipt', archived);
    // WatchChats republishes asynchronously after a registry write; poll the readback.
    const rows = await until(async () => { const current = await call('WatchChats'); return current.find(row => row.id === parent)?.archived ? current : null; });
    save('archived-parent-readback', rows);
    assert.ok(rows.find(row => row.id === parent).archived); assert.ok(!rows.find(row => row.id === child).archived);
    await assert.rejects(call('CreateChildThread', { parentTaskId: parent, request: { ...request, idempotencyKey: 'archived-parent' } }));
  });
  // ---- Codex MCP path: the same child tool reached through `luna` MCP ------
  await stop();
  await start({ LUNA_DATA_DIR: codexData, LUNA_HARNESS: 'codex', CODEX_HOME: codexHome, RUST_LOG: 'info,luna_harness::codex=debug',
    CODEX_EXECUTABLE: path.join(root, 'scripts/fixtures/luna-mcp-codex.py') });
  const mcpFrames = () => { const file = path.join(codexHome, 'luna-mcp-frames.jsonl'); return fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : []; };
  const codexInfo = await call('EngineInfo');
  const codexSpace = crypto.randomUUID(); const codexParent = crypto.randomUUID();
  const codexConfig = { harness: 'codex', model: null, reasoning: null, sandbox: 'read-only', permissionMode: 'ask', effectivePermissionMode: 'ask', modelOptions: {} };
  await call('Mutate', { op: 'createSpace', spaceId: codexSpace, deviceId: codexInfo.deviceId, path: project });
  await call('Mutate', { op: 'createChat', chatId: codexParent, spaceId: codexSpace, cwd: project, config: codexConfig });
  const runCodex = async prompt => {
    const { snapshot } = await call('GetTaskCommandSnapshot', { taskId: codexParent });
    return call('ExecuteTaskCommand', { commandId: crypto.randomUUID(), taskId: codexParent,
      expectedTaskRevision: snapshot.taskRevision, expectedBindingGeneration: snapshot.bindingGeneration,
      intent: { kind: 'session', command: { kind: 'run', request: { prompt, harness: 'codex', model: null, reasoning: null,
        cwd: project, sandbox: 'read-only', permissionMode: 'ask', resume: null }, messageId: crypto.randomUUID() } } });
  };
  const sessionStatus = async () => (await call('WatchSessions')).find(item => item.chatId === codexParent)?.status;
  await check('codex MCP: endpoint lists LUNA tools, rejects a wrong token and a foreign thread', async () => {
    save('codex-run-1-admission', await runCodex('[mcp-qa:first] Create one child chat.'));
    const frames = await until(async () => { const all = mcpFrames(); return all.some(frame => frame.label === 'foreign-thread') ? all : null; });
    const start = frames.find(frame => frame.label === 'thread/start');
    assert.ok(start?.mcpUrlPresent && start.tokenPresent && !start.dynamicTools, JSON.stringify(start));
    const names = frames.find(frame => frame.label === 'tools/list').response.result.tools.map(tool => tool.name);
    for (const name of ['create_thread', 'list_threads', 'luna_status']) assert.ok(names.includes(name), names.join(','));
    assert.equal(frames.find(frame => frame.label === 'list_threads').response.result.isError, false);
    assert.equal(frames.find(frame => frame.label === 'wrong-token').status, 401);
    const foreign = frames.find(frame => frame.label === 'foreign-thread').response.result;
    assert.ok(foreign.isError && foreign.content[0].text.includes('not attached'), JSON.stringify(foreign));
  });
  await check('codex MCP: create_thread parks on local approval past 40 s; Stop retires it without a child', async () => {
    await until(async () => mcpFrames().some(frame => frame.label === 'create_thread:sent'));
    const samples = [];
    const sample = async () => { samples.push({ atMs: Date.now(), status: await sessionStatus(), answered: mcpFrames().some(frame => frame.label === 'create_thread') }); };
    try {
      await until(async () => { await sample(); return ['awaitingApproval', 'awaitingInput'].includes(samples.at(-1).status); }, 20000);
    } finally { save('codex-park-status-samples', samples); }
    const parkedAt = Date.now();
    while (Date.now() - parkedAt < 45000) { await sample(); await new Promise(resolve => setTimeout(resolve, 3000)); }
    save('codex-park-status-samples', samples);
    const parked = samples.filter(item => item.atMs >= parkedAt);
    assert.ok(parked.every(item => ['awaitingApproval', 'awaitingInput'].includes(item.status) && !item.answered), `approval did not stay parked: ${JSON.stringify(parked)}`);
    const { snapshot } = await call('GetTaskCommandSnapshot', { taskId: codexParent });
    const stop = await call('ExecuteTaskCommand', { commandId: crypto.randomUUID(), taskId: codexParent,
      expectedBindingGeneration: snapshot.bindingGeneration, expectedTurnId: snapshot.activeTurnId,
      intent: { kind: 'session', command: { kind: 'interrupt' } } });
    save('codex-stop-receipt', { snapshot, stop });
    const created = await until(async () => mcpFrames().find(frame => frame.label === 'create_thread'), 20000)
      .catch(async error => { save('codex-after-stop-sessions', await call('WatchSessions')); throw error; });
    save('codex-create-after-stop', { parkedMs: Date.now() - parkedAt, frame: created });
    assert.equal(created.response.result.isError, true, JSON.stringify(created));
    await until(async () => !['working', 'awaitingApproval', 'awaitingInput'].includes(await sessionStatus()));
    const chats = await call('WatchChats');
    assert.ok(!chats.some(chat => chat.childThread?.parentTaskId === codexParent), 'Stop must not create a child');
  });
  await check('codex MCP: the resumed chat gets the tools again on its next spawn', async () => {
    const before = mcpFrames().length;
    save('codex-run-2-admission', await runCodex('[mcp-qa:resume] List the other chats.'));
    const frames = await until(async () => { const fresh = mcpFrames().slice(before); return fresh.some(frame => frame.label === 'list_threads') ? fresh : null; });
    const resumed = frames.find(frame => frame.label === 'thread/resume');
    assert.ok(resumed?.mcpUrlPresent && resumed.tokenPresent, JSON.stringify(frames[0]));
    assert.ok(frames.find(frame => frame.label === 'tools/list').response.result.tools.some(tool => tool.name === 'create_thread'));
    assert.equal(frames.find(frame => frame.label === 'list_threads').response.result.isError, false);
    await until(async () => !['working', 'awaitingApproval', 'awaitingInput'].includes(await sessionStatus()));
  });
} catch (error) { failure = String(error.stack || error); console.error(failure); process.exitCode = 1; }
finally {
  try { await stop(); } catch (error) { failure ??= String(error); process.exitCode = 1; }
  const peerFrames = path.join(codexHome, 'luna-mcp-frames.jsonl');
  if (fs.existsSync(peerFrames)) fs.copyFileSync(peerFrames, path.join(evidence, 'codex-mcp-frames.jsonl'));
  await new Promise(resolve => log.end(resolve));
  save('result', { status: failure ? 'FAIL' : 'PASS', checks, failure, parent, space, port, binary, binarySha256: hash(binary), sourceCommit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), evidence, proof: 'actual isolated engine with synthetic Mock provider; real Codex tool/approval and native UI acceptance remain separate gates' });
  fs.writeFileSync(path.join(evidence, 'SHA256SUMS'), fs.readdirSync(evidence).filter(name => name.endsWith('.json') || name.endsWith('.jsonl') || name.endsWith('.log')).map(name => `${hash(path.join(evidence, name))}  ${name}`).join('\n') + '\n');
  console.log(`Evidence: ${evidence}`);
}
