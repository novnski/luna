#!/usr/bin/env node
// Disposable signed app -> scheduler worker -> ordinary native Codex/Claude run.
// Save every attempted mutation before sending. Unknown work is never replayed.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const base = path.join(root, 'target/qa/harness-evolution/slice-5/native');
const bundle = path.join(root, 'target/luna-macos-dev/LUNA QA Automations.app');
const port = 59813;
const mode = process.argv[2] ?? 'run';
assert.ok(['run', 'verify-restart', 'inspect'].includes(mode));
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'}));
assert.equal(plist.CFBundleIdentifier, 'com.novnski.luna.qa.automations');
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, path.join(base, 'data'));
const pids = [...new Set(execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], {encoding: 'utf8'}).match(/^p\d+$/gm))];
assert.equal(pids.length, 1);
const pid = Number(pids[0].slice(1));
assert.equal(execFileSync('ps', ['-p', String(pid), '-o', 'comm='], {encoding: 'utf8'}).trim(), fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna')));
fs.mkdirSync(base, {recursive: true});
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
function call(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++seq;
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); ws.send(JSON.stringify({id, cancel: true})); reject(new Error(`${method} timed out; no automatic replay`)); }, 45000);
    function receive(event) {
      for (const line of String(event.data).split('\n').filter(Boolean)) {
        const frame = JSON.parse(line);
        if (frame.id !== id || !['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) continue;
        clearTimeout(timer); ws.removeEventListener('message', receive);
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({id, cancel: true}));
        if (frame.err) reject(new Error(JSON.stringify(frame.err)));
        else resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    }
    ws.addEventListener('message', receive); ws.send(JSON.stringify({id, method, params}));
  });
}
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn, label, ms = 180000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = await fn(); if (value) return value; await delay(300); }
  throw new Error(`Timed out: ${label}`);
}
let evidence;
const save = (name, value) => fs.writeFileSync(path.join(evidence, `${name}.json`), `${JSON.stringify(value, null, 2)}\n`);
const transcript = async taskId => { const value = await call('WatchDocMessages', {chatId: taskId}); return value.frame?.reset ?? value.reset ?? []; };
async function mutation(host, automationId, expectedRevision, operation, name) {
  const request = {requestId: crypto.randomUUID(), automationId, ownerHostId: host, expectedRevision, operation};
  const preview = await call('PreviewAutomation', request); request.previewId = preview.previewId;
  save(`${name}-attempt`, {request, preview});
  const receipt = await call('MutateAutomation', request); save(`${name}-receipt`, receipt);
  assert.deepEqual(await call('GetAutomationMutationReceipt', {requestId: request.requestId}), receipt);
  return receipt;
}
async function readback(manifest) {
  const results = {};
  for (const [provider, row] of Object.entries(manifest.tasks)) {
    const ledger = await call('ListAutomationRuns', {automationId: row.automationId});
    const paused = await call('ListAutomationRuns', {automationId: row.pausedId});
    const entries = await transcript(row.taskId);
    results[provider] = {ledger, paused, entries};
  }
  return results;
}
try {
  const info = await call('EngineInfo'); assert.equal(info.workspaceScope, 'local');
  if (mode === 'run') {
    evidence = fs.mkdtempSync(path.join(base, 'run-')); fs.writeFileSync(path.join(base, 'latest'), evidence);
    const project = path.join(evidence, 'project'); fs.mkdirSync(project); fs.writeFileSync(path.join(project, 'README.md'), 'Disposable automation QA. No file changes or external writes.\n');
    execFileSync('git', ['init', '-q', project]);
    const manifest = {pid, port, profileId: info.profileId, host: info.deviceId, tasks: {}, completed: false}; save('manifest', manifest);
    const spaceId = crypto.randomUUID(); await call('Mutate', {op: 'createSpace', spaceId, deviceId: info.deviceId, path: project});
    for (const provider of ['codex', 'claude-code']) {
      const catalog = await call('ListModels', {harness: provider}); save(`catalog-${provider}`, catalog);
      const preferred = provider === 'codex' ? ['gpt-6-luna', 'gpt-6.1-sol'] : ['claude-sonnet-4-6', 'claude-sonnet-4-5'];
      const model = preferred.map(id => catalog.find(m => m.id === id)).find(Boolean) ?? catalog.find(m => m.isDefault) ?? catalog[0]; assert.ok(model);
      const taskId = crypto.randomUUID(), automationId = crypto.randomUUID(), pausedId = crypto.randomUUID();
      const marker = `AUTOMATION_QA_${crypto.randomBytes(6).toString('hex')}`;
      const config = {harness: provider, model: model.id, reasoning: model.reasoningLevels?.includes('low') ? 'low' : model.reasoningLevels?.[0], permissionMode: 'ask', sandbox: 'workspace-write'};
      await call('Mutate', {op: 'createChat', chatId: taskId, spaceId, cwd: project, config, title: `Automation QA ${provider}`});
      // createChat returns before Git checkout discovery. A schedule approved
      // with no checkout identity is correctly refused if that identity changes.
      // Wait for this disposable Git project's actual identity before review.
      const target = await until(async () => {
        const row = (await call('WatchChats')).find(chat => chat.id === taskId);
        return row?.checkoutId && row.cwd === project && row;
      }, `${provider} checkout identity`, 30000);
      save(`${provider}-target-before-review`, target);
      const atMs = Date.now() + 5000, pausedAtMs = atMs + 5000;
      manifest.tasks[provider] = {taskId, automationId, pausedId, marker, atMs, pausedAtMs, config}; save('manifest', manifest);
      const definition = {name: `QA ${provider}`, prompt: `Reply with exactly ${marker}. Do not use tools, change files, contact any service or create work.`, timezone: 'Europe/Bucharest', target: {kind: 'existing', taskId}, trigger: {kind: 'once', atMs}, maxLatenessMs: 120000};
      await mutation(info.deviceId, automationId, 0, {kind: 'save', definition}, `${provider}-save`);
      await mutation(info.deviceId, pausedId, 0, {kind: 'save', definition: {...definition, name: `Paused QA ${provider}`, prompt: `PAUSED_${marker}`, trigger: {kind: 'once', atMs: pausedAtMs}}}, `${provider}-paused-save`);
      await mutation(info.deviceId, pausedId, 1, {kind: 'pause'}, `${provider}-pause-before-due`);
      const ledger = await until(async () => {
        const page = await call('ListAutomationRuns', {automationId});
        if (page.items.some(run => ['failed', 'unknown', 'skipped', 'cancelled'].includes(run.state))) { save(`${provider}-unexpected-ledger`, page); throw new Error(`${provider}: terminal occurrence was not completed`); }
        return page.items.length === 1 && page.items[0].state === 'completed' && page;
      }, `${provider} one native scheduled run`);
      const run = ledger.items[0]; assert.equal(run.scheduledAtMs, atMs); assert.equal(run.coalesced, 0); assert.equal(run.taskId, taskId);
      const entries = await transcript(taskId); assert.equal(entries.filter(e => e.id === run.messageId && e.role === 'user').length, 1);
      assert.ok(entries.some(e => e.role === 'assistant' && (e.parts ?? []).some(p => p.kind === 'text' && p.text.includes(marker))));
      assert.equal(entries.filter(e => e.role === 'user').length, 1);
      manifest.tasks[provider].run = run; save('manifest', manifest); save(`${provider}-completed`, {ledger, entries});
      await mutation(info.deviceId, automationId, 1, {kind: 'pause'}, `${provider}-pause-completed`);
      await delay(Math.max(0, pausedAtMs + 2200 - Date.now()));
      assert.equal((await call('ListAutomationRuns', {automationId: pausedId})).items.length, 0);
    }
    manifest.completed = true; save('manifest', manifest); save('result', {passed: true, pid, results: await readback(manifest)});
    console.log(JSON.stringify({passed: true, evidence, providers: Object.keys(manifest.tasks), pid}));
  } else {
    evidence = fs.readFileSync(path.join(base, 'latest'), 'utf8').trim(); assert.ok(evidence.startsWith(`${base}/run-`));
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8'));
    if (mode === 'inspect') { save(`inspection-${Date.now()}`, await readback(manifest)); console.log(JSON.stringify({evidence, inspected: true, pid})); }
    else {
      assert.equal(manifest.completed, true); assert.notEqual(pid, manifest.pid); await delay(4200);
      const results = await readback(manifest);
      for (const [provider, row] of Object.entries(manifest.tasks)) {
        assert.equal(results[provider].ledger.items.length, 1); assert.equal(results[provider].ledger.items[0].id, row.run.id); assert.equal(results[provider].ledger.items[0].state, 'completed');
        assert.equal(results[provider].paused.items.length, 0); assert.equal(results[provider].entries.filter(e => e.role === 'user').length, 1);
      }
      save('restart', {passed: true, pid, results}); console.log(JSON.stringify({passed: true, evidence, restarted: true, pid}));
    }
  }
} catch (error) {
  if (evidence) { save(`failure-${mode}-${Date.now()}`, {error: String(error)}); try { const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8')); save(`failure-readback-${Date.now()}`, await readback(manifest)); } catch {} }
  console.error(String(error)); process.exitCode = 1;
} finally { ws.close(); }
