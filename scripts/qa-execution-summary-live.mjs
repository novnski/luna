#!/usr/bin/env node
// Real signed app -> owning engine -> native provider. Disposable QA identity
// only; preserves every attempted run and never retries an uncertain send.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const base = path.join(root, 'target/qa/harness-evolution/native');
const bundle = path.join(root, 'target/luna-macos-dev/LUNA QA Harness.app');
const port = 59809;
const provider = process.argv[2] ?? 'codex';
assert.ok(['codex', 'claude-code', 'verify-restart'].includes(provider));
const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'}));
assert.equal(plist.CFBundleIdentifier, 'com.novnski.luna.qa.harness');
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, path.join(base, 'data'));
const pids = execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], {encoding: 'utf8'}).match(/^p\d+$/gm);
assert.equal(new Set(pids).size, 1);
const pid = Number(pids[0].slice(1));
assert.equal(execFileSync('ps', ['-p', String(pid), '-o', 'comm='], {encoding: 'utf8'}).trim(), executable);
fs.mkdirSync(base, {recursive: true});
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
const call = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++seq;
  const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 45000);
  function receive(event) {
    for (const line of String(event.data).split('\n').filter(Boolean)) {
      const frame = JSON.parse(line);
      if (frame.id !== id || !['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) continue;
      clearTimeout(timer); ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(JSON.stringify(frame.err)));
      else {
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({id, cancel: true}));
        resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    }
  }
  ws.addEventListener('message', receive); ws.send(JSON.stringify({id, method, params}));
});
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn, description, ms = 120000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) { const result = await fn(); if (result) return result; await delay(150); }
  throw new Error(`Timed out: ${description}`);
}
const save = (directory, name, value) => fs.writeFileSync(path.join(directory, `${name}.json`), `${JSON.stringify(value, null, 2)}\n`);
const rows = value => value.frame?.reset ?? value.reset ?? [];
const text = entry => (entry.parts ?? []).filter(part => part.kind === 'text').map(part => part.text).join('\n');
const transcript = async chatId => rows(await call('WatchDocMessages', {chatId}));
const summary = async taskId => (await call('GetRuntimeContract', {taskId})).execution;
let evidence;
try {
  const info = await call('EngineInfo');
  assert.equal(info.workspaceScope, 'local');
  if (provider === 'verify-restart') {
    const results = [];
    for (const directory of fs.readdirSync(base).filter(name => name.startsWith('run-'))) {
      const file = path.join(base, directory, 'result.json');
      if (!fs.existsSync(file)) continue;
      const prior = JSON.parse(fs.readFileSync(file, 'utf8'));
      if (!prior.passed) continue;
      assert.notEqual(pid, prior.pid, 'Restart the exact QA app first');
      const current = await summary(prior.taskId);
      assert.equal(current.running, null);
      assert.ok(['idle', 'unavailable'].includes(current.phase));
      const entries = await transcript(prior.taskId);
      assert.equal(entries.filter(entry => entry.id === prior.queuedId).length, 1);
      results.push({taskId: prior.taskId, provider: prior.provider, execution: current});
    }
    assert.ok(results.length >= 2, 'Both live provider runs must pass first');
    save(base, 'restart', {pid, results, passed: true});
    console.log(`PASS restart: ${results.length} tasks have no stale live observation`);
  } else {
    evidence = fs.mkdtempSync(path.join(base, `run-${provider}-`));
    const project = path.join(evidence, 'project'); fs.mkdirSync(project);
    fs.writeFileSync(path.join(project, 'README.md'), 'Isolated execution-observation QA. Do not change files.\n');
    execFileSync('git', ['init', '-q', project]);
    const catalog = await call('ListModels', {harness: provider}); save(evidence, 'catalog', catalog);
    const preferred = provider === 'codex' ? ['gpt-6-luna', 'gpt-6.1-sol'] : ['claude-sonnet-4-6', 'claude-sonnet-4-5'];
    const model = preferred.map(id => catalog.find(model => model.id === id)).find(Boolean) ?? catalog.find(model => model.isDefault) ?? catalog[0];
    assert.ok(model, 'Live catalog has no available model');
    const modelOptions = {};
    for (const option of model.options ?? []) if (option.id === 'serviceTier' && option.choices.some(choice => choice.id === 'default')) modelOptions.serviceTier = 'default';
    const config = {harness: provider, model: model.id, reasoning: model.reasoningLevels?.includes('low') ? 'low' : model.reasoningLevels?.[0], modelOptions, permissionMode: 'full', effectivePermissionMode: 'full', sandbox: 'danger-full-access'};
    const taskId = crypto.randomUUID(), spaceId = crypto.randomUUID(), firstId = crypto.randomUUID(), queuedId = crypto.randomUUID();
    const marker = `EXECUTION_QA_${crypto.randomUUID().slice(0, 8)}`;
    const manifest = {provider, taskId, spaceId, firstId, queuedId, marker, model: model.id, project, pid, profileId: info.profileId};
    save(evidence, 'manifest', manifest);
    await call('Mutate', {op: 'createSpace', spaceId, deviceId: info.deviceId, path: project});
    await call('Mutate', {op: 'createChat', chatId: taskId, spaceId, cwd: project, config});
    await call('Mutate', {op: 'renameChat', chatId: taskId, title: `QA execution ${provider}`});
    await call('QueueMessage', {chatId: taskId, messageId: firstId, text: `This is application QA. Run exactly one harmless shell command: sleep 15; printf '${marker}_FIRST\\n'. Do not read or modify files and do not use other tools. After it finishes, reply with ${marker}_FIRST.`, holdForTurnEnd: true});
    const active = await until(async () => {
      const execution = await summary(taskId);
      return execution?.phase === 'running' && execution.running && execution;
    }, 'native session start');
    assert.equal(active.running.permissionMode, 'full');
    assert.deepEqual(active.pending, []);
    save(evidence, 'running', active);
    await call('Mutate', {op: 'setThreadPermissionMode', chatId: taskId, mode: 'ask', actor: 'user'});
    const pending = await summary(taskId);
    assert.equal(pending.phase, 'running');
    assert.equal(pending.running.permissionMode, 'full');
    assert.equal(pending.requested.permissionMode, 'ask');
    assert.equal(pending.requested.sandbox, 'workspace-write');
    assert.ok(pending.pending.includes('permission'));
    assert.equal(pending.steer.kind, 'unavailable');
    assert.equal(pending.transition.kind, 'restartAndResume');
    save(evidence, 'pending', pending);
    const watched = (await call('WatchChats')).find(chat => chat.id === taskId).execution;
    assert.deepEqual(watched.pending, pending.pending);
    assert.deepEqual(watched.running, pending.running);
    await call('QueueMessage', {chatId: taskId, messageId: queuedId, text: `Do not use tools. Reply exactly ${marker}_SECOND.`, followUpBehavior: 'steer', holdForTurnEnd: true});
    const held = await until(async () => {
      const item = (await call('WatchQueue', {chatId: taskId})).items.find(item => item.id === queuedId);
      return item?.holdReason === 'steeringUnavailable' && item;
    }, 'pending steer held');
    save(evidence, 'held', held);
    assert.ok(!(await transcript(taskId)).some(entry => entry.id === queuedId));
    await until(async () => {
      const execution = await summary(taskId);
      const entries = await transcript(taskId);
      return execution.phase !== 'running' && execution.phase !== 'starting' && entries.some(entry => entry.role === 'assistant' && entry.status === 'complete' && text(entry).includes(`${marker}_FIRST`));
    }, 'first native response');
    assert.ok((await call('WatchQueue', {chatId: taskId})).items.some(item => item.id === queuedId));
    const sent = await call('SendQueuedMessageNow', {chatId: taskId, id: queuedId, text: ''});
    assert.ok(sent.sent); save(evidence, 'send-receipt', sent);
    const applied = await until(async () => {
      const execution = await summary(taskId);
      return execution.running?.permissionMode === 'ask' && execution;
    }, 'new native runtime applies Ask');
    assert.deepEqual(applied.pending, []); save(evidence, 'applied', applied);
    const completed = await until(async () => {
      const entries = await transcript(taskId);
      const at = entries.findIndex(entry => entry.id === queuedId);
      const execution = await summary(taskId);
      return at >= 0 && !['running', 'starting'].includes(execution.phase) && entries.slice(at + 1).some(entry => entry.role === 'assistant' && entry.status === 'complete' && text(entry).includes(`${marker}_SECOND`)) && entries;
    }, 'second native response');
    assert.equal(completed.filter(entry => entry.id === queuedId).length, 1);
    save(evidence, 'transcript', completed);
    save(evidence, 'result', {...manifest, passed: true, binarySha256: crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex'), completedAt: new Date().toISOString()});
    console.log(`PASS ${provider}: pending Full → Ask blocks Steer and applies exactly once after the turn. Evidence: ${evidence}`);
  }
} catch (error) {
  if (evidence) save(evidence, 'failure', {error: error.stack, pid, at: new Date().toISOString()});
  throw error;
} finally { ws.close(); }
