#!/usr/bin/env node
// Coordinator-owned real-provider E2E. Creates an ordinary disposable repo;
// never creates worktrees, builds, quits apps, or reads provider auth files.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import readline from 'node:readline';
import { spawn, execFileSync } from 'node:child_process';

const [mode, directory] = process.argv.slice(2);
assert.ok(['run', 'snapshot'].includes(mode), 'usage: qa-same-chat-rewind.mjs run|snapshot /absolute/evidence-directory');
assert.ok(directory && path.isAbsolute(directory), 'evidence directory must be absolute');
const evidence = path.resolve(directory);
const codexHome = process.env.LUNA_REWIND_CODEX_HOME;
assert.ok(codexHome && path.isAbsolute(codexHome), 'set LUNA_REWIND_CODEX_HOME to the explicit native account home used by LUNA');
const port = process.env.LUNA_REWIND_IPC_PORT ?? '59777';
const root = path.resolve(import.meta.dirname, '..');
const app = process.env.LUNA_REWIND_APP ?? path.join(root, 'target/luna-macos-dev/LUNA Dev.app');
execFileSync('codesign', ['--verify', '--deep', '--strict', app]);
const executable = fs.realpathSync(path.join(app, 'Contents/MacOS/luna'));
const pid = execFileSync('lsof', ['-t', '-nP', `-iTCP:${port}`, '-sTCP:LISTEN'], { encoding: 'utf8' }).trim();
assert.match(pid, /^\d+$/, 'exactly one selected LUNA app must listen');
assert.equal(execFileSync('ps', ['-p', pid, '-o', 'command='], { encoding: 'utf8' }).trim(), executable);
if (mode === 'run') {
  assert.ok(!fs.existsSync(evidence), 'run directory must be fresh');
  fs.mkdirSync(evidence, { recursive: true });
} else assert.ok(fs.existsSync(path.join(evidence, 'manifest.json')), 'run first');
const stamp = mode === 'run' ? 'run' : `snapshot-${Date.now()}`;
const trace = [];
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timeout`)); }, 45000);
    function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer); ws.removeEventListener('message', receive);
      if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
      const value = Object.hasOwn(frame, 'item') ? frame.item : frame.ok;
      trace.push({ method, params, value, error: frame.err });
      if (frame.err) reject(new Error(`${method}: ${JSON.stringify(frame.err)}`)); else resolve(value);
    }
    ws.addEventListener('message', receive);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
function save(name, value) { fs.writeFileSync(path.join(evidence, name), JSON.stringify(value, null, 2) + '\n', { flag: 'wx' }); }
function git(cwd, args) {
  return execFileSync('git', ['-C', cwd, ...args], { encoding: 'utf8', env: {
    ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_AUTHOR_NAME: 'LUNA QA', GIT_AUTHOR_EMAIL: 'qa@example.invalid',
    GIT_COMMITTER_NAME: 'LUNA QA', GIT_COMMITTER_EMAIL: 'qa@example.invalid',
  } }).trim();
}
function entries(value) { return value?.frame?.reset ?? value?.reset ?? []; }
function text(entry) { return (entry?.parts ?? []).filter(part => part.kind === 'text').map(part => part.text.trim()).join('\n'); }
function fileHashes(cwd) {
  return Object.fromEntries(['README.md', 'unrelated.txt'].map(name => [name,
    crypto.createHash('sha256').update(fs.readFileSync(path.join(cwd, name))).digest('hex')]));
}
async function execute(taskId, intent, commandId = crypto.randomUUID(), snapshot) {
  snapshot ??= (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
  const params = { commandId, taskId, expectedTaskRevision: snapshot.taskRevision,
    expectedBindingGeneration: snapshot.bindingGeneration, intent };
  let receipt = await call('ExecuteTaskCommand', params);
  for (let i = 0; receipt?.status === 'pending' && i < 45; i++) {
    await new Promise(resolve => setTimeout(resolve, 1000));
    receipt = await call('GetTaskCommandReceipt', { commandId });
  }
  return { params, receipt };
}
async function nativeTurns(threadId, cwd) {
  const child = spawn(process.env.LUNA_REWIND_CODEX ?? 'codex', ['app-server'], {
    cwd, env: { ...process.env, CODEX_HOME: codexHome }, stdio: ['pipe', 'pipe', 'ignore'],
  });
  const pending = new Map(); let id = 1;
  const lines = readline.createInterface({ input: child.stdout });
  lines.on('line', line => {
    let frame; try { frame = JSON.parse(line); } catch { return; }
    const handler = pending.get(frame.id); if (!handler) return;
    pending.delete(frame.id); clearTimeout(handler.timer);
    if (frame.error) handler.reject(new Error(JSON.stringify(frame.error))); else handler.resolve(frame.result);
  });
  child.on('error', error => { for (const handler of pending.values()) handler.reject(error); });
  function rpc(method, params) {
    const requestId = id++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`native ${method} timeout`)); }, 30000);
      pending.set(requestId, { resolve, reject, timer });
      child.stdin.write(JSON.stringify({ id: requestId, method, params }) + '\n');
    });
  }
  try {
    await rpc('initialize', { clientInfo: { name: 'luna-rewind-qa', title: 'LUNA rewind QA', version: '1' }, capabilities: { experimentalApi: true } });
    child.stdin.write(JSON.stringify({ method: 'initialized' }) + '\n');
    const metadata = await rpc('thread/read', { threadId, includeTurns: false });
    assert.equal(metadata.thread.id, threadId);
    assert.equal(metadata.thread.historyMode, 'paginated');
    const turns = []; let cursor = null; const cursors = new Set();
    for (let page = 0; page < 100; page++) {
      const value = await rpc('thread/turns/list', { threadId, cursor, limit: 100, sortDirection: 'asc', itemsView: 'full' });
      turns.push(...value.data); cursor = value.nextCursor;
      if (cursor === null) return turns;
      assert.equal(typeof cursor, 'string'); assert.ok(!cursors.has(cursor)); cursors.add(cursor);
    }
    throw new Error('native history exceeds page bound');
  } finally {
    child.stdin.end(); lines.close();
    for (const handler of pending.values()) clearTimeout(handler.timer);
    // Only this disposable read-only app-server child is terminated.
    child.kill('SIGTERM');
  }
}
async function snapshot(manifest, name) {
  const { taskId, cwd } = manifest;
  const identity = await call('GetTaskIdentity', { taskId });
  const messages = await call('WatchDocMessages', { chatId: taskId });
  const draft = await call('GetComposerDraft', { taskId });
  const checkpoints = await call('ListGitCheckpoints', { chatId: taskId, cwd });
  const turns = await nativeTurns(identity.currentBinding.providerTaskId, cwd);
  const result = { identity, messages, draft, checkpoints, turns, fileHashes: fileHashes(cwd), pid, executable };
  save(`${name}.json`, result); return result;
}
try {
  if (mode === 'snapshot') {
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8'));
    const result = await snapshot(manifest, stamp);
    assert.equal(result.identity.currentBinding.providerTaskId, manifest.providerTaskId);
    assert.deepEqual(result.fileHashes, manifest.fileHashes);
    console.log(JSON.stringify({ result: 'readback-recorded', evidence, taskId: manifest.taskId }));
  } else {
    const cwd = path.join(evidence, 'workspace'); fs.mkdirSync(cwd);
    git(cwd, ['init', '-q', '-b', 'main']);
    fs.writeFileSync(path.join(cwd, 'README.md'), 'LUNA rewind disposable ordinary repository.\n');
    git(cwd, ['add', 'README.md']); git(cwd, ['commit', '-q', '-m', 'QA fixture']);
    async function mutate(params) {
      const result = await call('Mutate', params);
      let receipt = result.receipt;
      for (let i = 0; receipt?.status === 'pending' && i < 45; i++) {
        await new Promise(resolve => setTimeout(resolve, 1000));
        receipt = await call('GetTaskCommandReceipt', { commandId: receipt.commandId });
      }
      if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
    }
    const info = await call('EngineInfo');
    const taskId = crypto.randomUUID(); const spaceId = crypto.randomUUID();
    const markers = Array.from({ length: 3 }, () => `LUNA_REWIND_${crypto.randomBytes(6).toString('hex')}`);
    const model = process.env.LUNA_REWIND_MODEL ?? 'gpt-6.1-sol';
    const reasoning = process.env.LUNA_REWIND_REASONING ?? 'high';
    const catalog = await call('ListModels', { harness: 'codex', refresh: true });
    const selectedModel = catalog.find(item => item.id === model);
    assert.ok(selectedModel, `Codex model ${model} must be advertised`);
    assert.ok(selectedModel.reasoningLevels?.includes(reasoning), `${model} must advertise ${reasoning} reasoning`);
    const config = { harness: 'codex', providerAccountId: 'codex-native', model, reasoning,
      sandbox: 'read-only', permissionMode: 'ask', effectivePermissionMode: 'ask' };
    await mutate({ op: 'createSpace', spaceId, deviceId: info.deviceId, path: cwd, name: 'Same-chat rewind QA', gitDetected: true });
    await mutate({ op: 'createChat', chatId: taskId, spaceId, cwd, config });
    await mutate({ op: 'renameChat', chatId: taskId, title: 'Same-chat rewind QA' });
    save('setup.json', { taskId, spaceId, cwd, model, reasoning, markers, executable, pid });
    async function turn(prompt, expected) {
      const messageId = crypto.randomUUID();
      const { receipt } = await execute(taskId, { kind: 'session', command: { kind: 'run', messageId,
        request: { prompt, cwd, ...config, resume: null } } });
      assert.ok(['applied', 'pending'].includes(receipt.status), JSON.stringify(receipt));
      for (let i = 0; i < 180; i++) {
        await new Promise(resolve => setTimeout(resolve, 1000));
        const messages = await call('WatchDocMessages', { chatId: taskId });
        const read = await call('GetTaskCommandSnapshot', { taskId });
        const last = entries(messages).filter(entry => entry.role === 'assistant').at(-1);
        if (!read.snapshot.activeTurnId && last?.status === 'complete' && text(last) === expected) return messageId;
      }
      throw new Error('real provider turn did not settle with expected marker');
    }
    const ids = [];
    for (const marker of markers) ids.push(await turn(`Remember this secret marker: ${marker}. Reply with exactly ${marker}. Do not use tools, read files, change files or spawn agents.`, marker));
    fs.appendFileSync(path.join(cwd, 'README.md'), 'Unrelated modified tracked content.\n');
    fs.writeFileSync(path.join(cwd, 'unrelated.txt'), 'Unrelated untracked content.\n');
    await call('SaveComposerDraft', { draft: { version: 1, draftId: crypto.randomUUID(), taskId,
      revision: 1, text: 'Unsent draft must survive rewind', updatedAtMs: Date.now() } });
    const manifest = { taskId, spaceId, cwd, model, reasoning, ids, markers, pid, executable };
    const before = await snapshot(manifest, 'before');
    assert.equal(before.turns.length, 3);
    manifest.providerTaskId = before.identity.currentBinding.providerTaskId;
    manifest.fileHashes = before.fileHashes;
    save('manifest.json', manifest);
    const read = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
    const user = entries(before.messages).find(entry => entry.id === ids[1]); assert.ok(user);
    const review = { taskRevision: read.taskRevision, bindingGeneration: read.bindingGeneration,
      providerSessionId: manifest.providerTaskId, providerAccountId: 'codex-native', cwd,
      messageId: ids[1], messageText: text(user) };
    const rewind = await execute(taskId, { kind: 'rewindConversation', messageId: ids[1], keepFiles: true, review }, crypto.randomUUID(), read);
    save('rewind-command.json', rewind); assert.equal(rewind.receipt.status, 'applied', JSON.stringify(rewind.receipt));
    const duplicate = await call('ExecuteTaskCommand', rewind.params);
    assert.equal(duplicate.status, 'applied');
    const after = await snapshot(manifest, 'after');
    assert.equal(after.identity.currentBinding.bindingId, before.identity.currentBinding.bindingId);
    assert.equal(after.identity.currentBinding.providerTaskId, manifest.providerTaskId);
    assert.deepEqual(after.turns.map(turn => turn.id), before.turns.slice(0, 1).map(turn => turn.id));
    assert.deepEqual(entries(after.messages).filter(entry => entry.role !== 'system'), entries(before.messages).filter(entry => entry.role !== 'system').slice(0, 2));
    assert.deepEqual(after.draft, before.draft); assert.deepEqual(after.fileHashes, before.fileHashes);
    assert.deepEqual(after.checkpoints, before.checkpoints);
    await turn('What secret marker was in the first message of this conversation? Reply only with that exact marker. Do not use tools.', markers[0]);
    const continued = await snapshot(manifest, 'continued');
    assert.equal(continued.identity.currentBinding.providerTaskId, manifest.providerTaskId);
    assert.equal(continued.turns.length, 2);
    assert.deepEqual(continued.fileHashes, before.fileHashes);
    const emptyRead = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
    const firstUser = entries(continued.messages).find(entry => entry.id === ids[0]);
    const empty = await execute(taskId, { kind: 'rewindConversation', messageId: ids[0], keepFiles: true,
      review: { taskRevision: emptyRead.taskRevision, bindingGeneration: emptyRead.bindingGeneration,
        providerSessionId: manifest.providerTaskId, providerAccountId: 'codex-native', cwd,
        messageId: ids[0], messageText: text(firstUser) } }, crypto.randomUUID(), emptyRead);
    save('empty-prefix-command.json', empty); assert.equal(empty.receipt.status, 'applied', JSON.stringify(empty.receipt));
    const emptyResult = await snapshot(manifest, 'empty-prefix');
    assert.equal(emptyResult.turns.length, 0);
    assert.equal(entries(emptyResult.messages).filter(entry => entry.role !== 'system').length, 0);
    assert.equal(emptyResult.identity.currentBinding.providerTaskId, manifest.providerTaskId);
    await turn('Reply exactly EMPTY_REWIND_CONTINUED. Do not use tools.', 'EMPTY_REWIND_CONTINUED');
    const emptyContinued = await snapshot(manifest, 'empty-continued');
    assert.equal(emptyContinued.turns.length, 1);
    assert.equal(emptyContinued.identity.currentBinding.providerTaskId, manifest.providerTaskId);
    assert.deepEqual(emptyContinued.fileHashes, before.fileHashes);
    assert.deepEqual(emptyContinued.draft, before.draft);
    save('result.json', { result: 'passed', taskId, providerTaskId: manifest.providerTaskId,
      assertions: ['same binding/session', 'exact retained prefix', 'duplicate does not revert twice',
        'draft preserved', 'file hashes preserved', 'checkpoints preserved', 'real same-session continuation', 'first-turn empty prefix and continuation'],
      pending: ['native UI screenshots', 'busy/stale/legacy/steer/unknown/restart scenarios'] });
    console.log(JSON.stringify({ result: 'passed', evidence, taskId, pending: 'see result.json' }));
  }
} catch (error) {
  save(`${stamp}-failure.json`, { error: String(error), stack: error.stack }); throw error;
} finally {
  save(`${stamp}-rpc-trace.json`, trace);
  const hashes = Object.fromEntries(fs.readdirSync(evidence).filter(name => name.endsWith('.json')).map(name => [name,
    crypto.createHash('sha256').update(fs.readFileSync(path.join(evidence, name))).digest('hex')]));
  save(`${stamp}-hashes.json`, hashes); ws.close();
}
