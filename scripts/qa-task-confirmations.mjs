#!/usr/bin/env node
// Native E2E fixture for the task confirmation flow. Operates only on an
// explicitly supplied target/qa directory and the signed primary LUNA Dev app.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const [mode, directory, label] = process.argv.slice(2);
assert.ok(['setup', 'snapshot', 'delete-fixture'].includes(mode), 'usage: qa-task-confirmations.mjs setup|snapshot|delete-fixture target/qa/<run> [label]');
assert.ok(directory, 'supply an evidence directory');
const evidence = path.resolve(directory);
assert.ok(evidence.startsWith(path.join(root, 'target/qa') + path.sep), 'evidence must be under target/qa');
if (mode === 'setup') {
  assert.ok(!fs.existsSync(evidence), 'a setup run cannot be replayed');
  fs.mkdirSync(evidence, { recursive: true });
} else {
  assert.ok(fs.existsSync(path.join(evidence, 'manifest.json')), 'setup first');
  if (mode === 'snapshot') assert.match(label ?? '', /^[a-z0-9-]+$/, 'supply a snapshot label');
}
const app = path.join(root, 'target/luna-macos-dev/LUNA Dev.app');
const executable = fs.realpathSync(path.join(app, 'Contents/MacOS/luna'));
execFileSync('codesign', ['--verify', '--deep', '--strict', app]);
const pid = execFileSync('lsof', ['-t', '-nP', '-iTCP:59777', '-sTCP:LISTEN'], { encoding: 'utf8' }).trim();
assert.match(pid, /^\d+$/, 'one primary listener is required');
assert.equal(execFileSync('ps', ['-p', pid, '-o', 'command='], { encoding: 'utf8' }).trim(), executable);

const ws = new WebSocket('ws://127.0.0.1:59777');
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 30000);
    function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timeout);
      ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      else {
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
        resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    }
    ws.addEventListener('message', receive);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function mutate(params) {
  const result = await call('Mutate', params);
  let receipt = result.receipt;
  const deadline = Date.now() + 30000;
  while (receipt?.status === 'pending' && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 100));
    receipt = await call('GetTaskCommandReceipt', { commandId: receipt.commandId });
  }
  if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
}
function save(name, value) {
  const file = path.join(evidence, name);
  fs.writeFileSync(file, JSON.stringify(value, null, 2) + '\n', { flag: 'wx' });
}
function git(cwd, args) {
  return execFileSync('git', ['-C', cwd, ...args], { encoding: 'utf8', env: {
    ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_AUTHOR_NAME: 'LUNA QA', GIT_AUTHOR_EMAIL: 'qa@example.invalid',
    GIT_COMMITTER_NAME: 'LUNA QA', GIT_COMMITTER_EMAIL: 'qa@example.invalid',
  } }).trim();
}
function assistantText(item) {
  const entries = item?.frame?.reset ?? item?.reset ?? [];
  return entries.filter(entry => entry.role === 'assistant')
    .flatMap(entry => entry.parts ?? []).filter(part => part.kind === 'text')
    .map(part => part.text ?? '').join('\n').trim();
}
try {
  if (mode === 'setup') {
    const info = await call('EngineInfo');
    const catalog = await call('ListModels', { harness: 'codex', refresh: true });
    const model = catalog.find(item => item.id === 'gpt-6-luna');
    assert.ok(model, 'Codex gpt-6-luna must be available');
    assert.ok(model.reasoningLevels?.includes('high'), 'High reasoning must be supported');
    const workspace = path.join(evidence, 'workspace');
    fs.mkdirSync(workspace);
    git(workspace, ['init', '-q', '-b', 'main']);
    fs.writeFileSync(path.join(workspace, 'README.md'), 'Disposable LUNA task confirmation QA workspace.\n');
    git(workspace, ['add', 'README.md']);
    git(workspace, ['commit', '-q', '-m', 'QA fixture']);
    const spaceId = crypto.randomUUID();
    const tasks = Object.fromEntries(['archive', 'settle', 'delete'].map(action => [action, crypto.randomUUID()]));
    const marker = `LUNA_CONFIRM_QA_${crypto.randomBytes(5).toString('hex')}`;
    const manifest = { app, executable, pid: Number(pid), spaceId, tasks, workspace, marker,
      model: 'gpt-6-luna', reasoning: 'high', startedAt: new Date().toISOString() };
    save('manifest.json', manifest);
    await mutate({ op: 'createSpace', spaceId, deviceId: info.deviceId, path: workspace,
      name: 'Task confirmations QA', gitDetected: true });
    const config = { harness: 'codex', model: 'gpt-6-luna', reasoning: 'high',
      sandbox: 'read-only', permissionMode: 'ask', effectivePermissionMode: 'ask' };
    for (const [action, chatId] of Object.entries(tasks)) {
      await mutate({ op: 'createChat', chatId, spaceId, cwd: workspace, config });
      await mutate({ op: 'renameChat', chatId, title: `Confirm ${action} QA ${marker.slice(-5)}` });
    }
    const taskId = tasks.archive;
    const snapshot = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
    const prompt = `Reply with exactly ${marker}. Do not use tools, read files, change files, or spawn agents.`;
    const commandId = crypto.randomUUID();
    save('provider-command.json', { taskId, commandId, prompt, snapshot });
    const admission = await call('ExecuteTaskCommand', { commandId, taskId,
      expectedTaskRevision: snapshot.taskRevision,
      expectedBindingGeneration: snapshot.bindingGeneration,
      intent: { kind: 'session', command: { kind: 'run',
        request: { prompt, harness: 'codex', model: 'gpt-6-luna', reasoning: 'high',
          cwd: workspace, sandbox: 'read-only', permissionMode: 'ask', resume: null },
        messageId: crypto.randomUUID() } } });
    save('provider-admission.json', admission);
    assert.ok(['pending', 'applied'].includes(admission.status), JSON.stringify(admission));
    let observed;
    for (let attempt = 0; attempt < 90; attempt++) {
      await new Promise(resolve => setTimeout(resolve, 1000));
      const [messages, sessions] = await Promise.all([
        call('WatchDocMessages', { chatId: taskId }), call('WatchSessions'),
      ]);
      const text = assistantText(messages);
      const live = sessions.some(session => session.chatId === taskId
        && ['working', 'awaitingApproval', 'awaitingInput'].includes(session.status));
      if (text === marker && !live) { observed = { text, live, attempt: attempt + 1 }; break; }
    }
    assert.ok(observed, 'real GPT-6 Luna High turn did not finish with the exact marker');
    save('provider-result.json', observed);
    console.log(JSON.stringify({ result: 'setup-complete', evidence, tasks, model: model.id, reasoning: 'high' }));
  } else if (mode === 'delete-fixture') {
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8'));
    const taskId = manifest.tasks.delete;
    assert.ok((await call('WatchChats')).some(chat => chat.id === taskId), 'delete QA task must exist');
    const snapshot = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
    const commandId = crypto.randomUUID();
    save('delete-command.json', { taskId, commandId, snapshot });
    let receipt = await call('ExecuteTaskCommand', { commandId, taskId,
      expectedTaskRevision: snapshot.taskRevision,
      expectedBindingGeneration: snapshot.bindingGeneration,
      intent: { kind: 'delete' } });
    for (let attempt = 0; receipt.status === 'pending' && attempt < 80; attempt++) {
      await new Promise(resolve => setTimeout(resolve, 250));
      receipt = await call('GetTaskCommandReceipt', { commandId });
    }
    save('delete-receipt.json', receipt);
    assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
    let absent = false;
    for (let attempt = 0; attempt < 40; attempt++) {
      absent = !(await call('WatchChats')).some(chat => chat.id === taskId);
      if (absent) break;
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    assert.ok(absent, 'deleted QA task must disappear from WatchChats');
    console.log(JSON.stringify({ result: 'delete-fixture-complete', taskId, commandId, absent }));
  } else {
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8'));
    assert.equal(manifest.executable, executable, 'QA app executable changed');
    const chats = await call('WatchChats');
    const actions = {};
    for (const [action, taskId] of Object.entries(manifest.tasks)) {
      actions[action] = {
        chat: chats.find(chat => chat.id === taskId) ?? null,
        commandRead: await call('GetTaskCommandSnapshot', { taskId }).catch(error => ({ error: String(error) })),
      };
    }
    const settingsPath = path.join(root, 'target/luna-macos-dev/data/ui-settings.json');
    const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    const result = { label, at: new Date().toISOString(), appPid: Number(pid), actions,
      confirmations: Object.fromEntries(['confirmArchive', 'confirmSettle', 'confirmDelete']
        .map(key => [key, settings[key] ?? true])) };
    save(`snapshot-${label}.json`, result);
    console.log(JSON.stringify({ result: 'snapshot', evidence, label,
      states: Object.fromEntries(Object.entries(actions).map(([key, value]) =>
        [key, value.chat && { archived: value.chat.archived,
          settlementOverride: value.chat.settlementOverride }])),
      confirmations: result.confirmations }));
  }
} finally {
  ws.close();
}
