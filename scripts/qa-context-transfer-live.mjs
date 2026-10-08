#!/usr/bin/env node
// Real signed app -> owning engine -> native provider. Disposable QA identity
// only; preserves every attempted run and never retries an uncertain send.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const defaultBase = path.join(root, 'target/qa/harness-evolution/slice-2/native');
const identityKeys = ['LUNA_DEV_APP_NAME', 'LUNA_DEV_BUNDLE_ID', 'LUNA_DEV_IPC_PORT', 'LUNA_DEV_DATA_DIR'];
const suppliedIdentity = identityKeys.filter(key => Object.hasOwn(process.env, key));
assert.ok(suppliedIdentity.length === 0 || suppliedIdentity.length === 4, 'QA identity overrides require all four LUNA_DEV_* values');
const overridden = suppliedIdentity.length === 4;
for (const key of suppliedIdentity) assert.match(process.env[key], /^\S(?:[^\r\n]*\S)?$/, `${key} must be nonempty and single-line`);
const appName = overridden ? process.env.LUNA_DEV_APP_NAME : 'LUNA QA Context';
const bundleId = overridden ? process.env.LUNA_DEV_BUNDLE_ID : 'com.novnski.luna.qa.context';
const portValue = overridden ? process.env.LUNA_DEV_IPC_PORT : '59810';
assert.match(appName, /^LUNA QA [A-Za-z0-9][A-Za-z0-9 -]{0,48}$/);
assert.match(bundleId, /^com\.novnski\.luna\.qa\.[a-z][a-z0-9-]{0,47}$/);
assert.match(portValue, /^[1-9][0-9]{0,4}$/);
const port = Number(portValue);
assert.ok(port >= 1024 && port <= 65535 && port !== 59777, 'A separate QA IPC port is required');
const dataDir = overridden ? process.env.LUNA_DEV_DATA_DIR : path.join(defaultBase, 'data');
assert.ok(path.isAbsolute(dataDir));
const bundle = path.join(root, 'target/luna-macos-dev', `${appName}.app`);
if (overridden) {
  const target = path.join(root, 'target');
  assert.equal(fs.realpathSync(target), target, 'QA target must not redirect through symlinks');
  assert.equal(fs.realpathSync(dataDir), dataDir, 'QA data path must be canonical');
  assert.ok(dataDir.startsWith(`${target}${path.sep}`), 'QA data must be inside this checkout target');
  const staging = path.join(target, 'luna-macos-dev');
  assert.ok(dataDir !== staging && !dataDir.startsWith(`${staging}${path.sep}`), 'QA data must be outside bundle staging and primary data');
  assert.equal(fs.realpathSync(bundle), bundle, 'QA bundle must not redirect through symlinks');
}
const auditBase = process.env.LUNA_AUDIT_NATIVE_BASE;
if (auditBase !== undefined) assert.ok(path.isAbsolute(auditBase) && !/[\r\n]/.test(auditBase), 'LUNA_AUDIT_NATIVE_BASE must be an absolute single-line path');
const base = auditBase === undefined ? defaultBase : path.join(auditBase, 'context-transfer');
const mode = process.argv[2] ?? 'run';
assert.ok(['run', 'verify-restart', 'finish-readback'].includes(mode));
const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'}));
assert.equal(plist.CFBundleIdentifier, bundleId);
assert.equal(plist.CFBundleDisplayName, appName);
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, dataDir);
if (overridden) assert.equal(plist.LSEnvironment.LUNA_QA_MCP_ISOLATION, '1', 'QA override requires signed MCP isolation');
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
const identity = taskId => call('GetTaskIdentity', {taskId});
const transfers = async taskId => (await call('ListContextTransfers', {taskId})).items;
async function command(taskId, intent, commandId = crypto.randomUUID()) {
  const {snapshot} = await call('GetTaskCommandSnapshot', {taskId});
  return call('ExecuteTaskCommand', {taskId, commandId, actorId: 'qa-user', clientId: 'qa-context', clientSessionId: 'qa-context', expectedTaskRevision: snapshot.taskRevision, expectedBindingGeneration: snapshot.bindingGeneration, intent});
}
async function configFor(provider) {
  const catalog = await call('ListModels', {harness: provider});
  const preferred = provider === 'codex' ? ['gpt-6-luna', 'gpt-6.1-sol'] : ['claude-sonnet-5-5', 'claude-sonnet-4-6', 'claude-sonnet-4-5'];
  const requestedModel = process.env[provider === 'codex' ? 'LUNA_QA_CODEX_MODEL' : 'LUNA_QA_CLAUDE_MODEL'];
  const model = requestedModel === undefined ? preferred.map(id => catalog.find(model => model.id === id)).find(Boolean) ?? catalog.find(model => model.isDefault) ?? catalog[0] : catalog.find(model => model.id === requestedModel);
  assert.ok(model, requestedModel === undefined ? `${provider} has no live model` : `${provider} model override is not in the live catalog: ${requestedModel}`);
  save(evidence, `catalog-${provider}`, catalog);
  const modelOptions = {};
  for (const option of model.options ?? []) if (option.id === 'serviceTier' && option.choices.some(choice => choice.id === 'default')) modelOptions.serviceTier = 'default';
  return {harness: provider, model: model.id, reasoning: model.reasoningLevels?.includes('low') ? 'low' : model.reasoningLevels?.[0], modelOptions, permissionMode: 'full', effectivePermissionMode: 'full', sandbox: 'danger-full-access'};
}
async function completed(taskId, messageId, prompt) {
  const entries = await until(async () => {
    const entries = await transcript(taskId);
    const at = entries.findIndex(entry => entry.id === messageId);
    const phase = (await summary(taskId)).phase;
    return at >= 0 && !['starting', 'running'].includes(phase) && entries.slice(at + 1).some(entry => entry.role === 'assistant' && entry.status === 'complete') && entries;
  }, `turn ${messageId}`, 180000);
  assert.equal(entries.filter(entry => entry.id === messageId).length, 1);
  assert.equal(text(entries.find(entry => entry.id === messageId)), prompt, 'Canonical user text must not contain injected context');
  return entries;
}
async function turn(taskId, prompt, name) {
  const messageId = crypto.randomUUID();
  save(evidence, `${name}-attempt`, {taskId, messageId, prompt});
  await call('QueueMessage', {chatId: taskId, messageId, text: prompt, holdForTurnEnd: true});
  const entries = await completed(taskId, messageId, prompt);
  save(evidence, name, entries);
  return entries.slice(entries.findIndex(entry => entry.id === messageId) + 1).filter(entry => entry.role === 'assistant');
}
async function transfer(request, name) {
  const preview = await call('PreviewContextTransfer', request);
  save(evidence, `${name}-preview`, preview);
  assert.ok(preview.included.length);
  const receipt = await command(request.targetTaskId, {kind: 'applyContextTransfer', request, previewId: preview.previewId});
  save(evidence, `${name}-command`, receipt);
  assert.equal(receipt.status, 'applied');
  const record = await call('GetContextTransfer', {taskId: request.targetTaskId, transferId: preview.previewId});
  assert.equal(record.receipt.status, 'planned');
  save(evidence, `${name}-planned`, record);
  return preview;
}
const gitState = project => ({status: execFileSync('git', ['-C', project, 'status', '--porcelain=v1'], {encoding: 'utf8'}), refs: execFileSync('git', ['-C', project, 'show-ref', '--heads', '--tags'], {encoding: 'utf8'}), index: crypto.createHash('sha256').update(fs.readFileSync(path.join(project, '.git/index'))).digest('hex'), readme: crypto.createHash('sha256').update(fs.readFileSync(path.join(project, 'README.md'))).digest('hex')});
async function finishReadback(manifest) {
  const {parent, markerA, markerB, project, gitBefore} = manifest;
  for (const name of ['claude', 'codex-return']) {
    const attempt = JSON.parse(fs.readFileSync(path.join(evidence, `${name}-recall-attempt.json`), 'utf8'));
    const entries = await transcript(parent);
    const at = entries.findIndex(entry => entry.id === attempt.messageId);
    assert.equal(entries.filter(entry => entry.id === attempt.messageId).length, 1);
    assert.equal(text(entries[at]), attempt.prompt);
    const nextUser = entries.findIndex((entry, index) => index > at && entry.role === 'user');
    const replies = entries.slice(at + 1, nextUser < 0 ? undefined : nextUser);
    assert.ok(replies.some(reply => reply.role === 'assistant' && reply.status === 'complete' && text(reply).includes(markerA) && text(reply).includes(markerB)));
  }
  const observed = await identity(parent);
  const returned = JSON.parse(fs.readFileSync(path.join(evidence, 'codex-return-identity.json'), 'utf8'));
  assert.equal(observed.currentBinding.providerTaskId, returned.currentBinding.providerTaskId);
  const receipts = await transfers(parent);
  assert.equal(receipts.length, 3);
  assert.ok(receipts.every(receipt => receipt.status === 'consumed'));
  assert.deepEqual(gitState(project), gitBefore);
  // Ordinary provider turns create internal checkpoint refs. They are recorded
  // separately; the invariant is unchanged user branches, index and files.
  const checkpointRefs = execFileSync('git', ['-C', project, 'for-each-ref', '--format=%(objectname) %(refname)', 'refs/luna/checkpoints'], {encoding: 'utf8'});
  const result = {...manifest, passed: true, finalNativeId: observed.currentBinding.providerTaskId, transfers: receipts, checkpointRefs, binarySha256: crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex'), completedAt: new Date().toISOString()};
  save(evidence, 'result', result); save(base, 'latest-success', result);
  console.log(`PASS native fork merge + active parent + Codex → Claude → Codex recall. Evidence: ${evidence}`);
}
let evidence;
try {
  const info = await call('EngineInfo');
  assert.equal(info.workspaceScope, 'local');
  assert.ok(info.capabilities.includes('context-transfer-v1'));
  if (mode === 'finish-readback') {
    evidence = path.resolve(process.argv[3] ?? '');
    assert.equal(path.dirname(evidence), base);
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8'));
    assert.equal(pid, manifest.pid);
    await finishReadback(manifest);
  } else if (mode === 'verify-restart') {
    const prior = JSON.parse(fs.readFileSync(path.join(base, 'latest-success.json'), 'utf8'));
    evidence = prior.evidence;
    assert.notEqual(pid, prior.pid, 'Restart the exact QA app first');
    const before = await transfers(prior.parent);
    assert.ok(before.every(item => item.status === 'consumed'));
    const binding = await identity(prior.parent);
    assert.equal(binding.currentBinding.providerTaskId, prior.finalNativeId);
    assert.equal((await summary(prior.parent)).running, null);
    const replies = await turn(prior.parent, 'Do not use tools. Repeat both distinct QA code words from this conversation, and nothing else.', 'restart-recall');
    assert.ok(replies.some(reply => text(reply).includes(prior.markerA) && text(reply).includes(prior.markerB)));
    assert.deepEqual(await transfers(prior.parent), before, 'Consumed transfers must not be dispatched again');
    assert.equal((await identity(prior.parent)).currentBinding.providerTaskId, prior.finalNativeId);
    assert.deepEqual(gitState(prior.project), prior.gitBefore);
    save(evidence, 'restart-result', {passed: true, pid, taskId: prior.parent, transfers: before});
    console.log(`PASS native restart: same session, both markers recalled, no transfer reinjected. Evidence: ${evidence}`);
  } else {
    evidence = fs.mkdtempSync(path.join(base, 'run-'));
    const project = path.join(evidence, 'project'); fs.mkdirSync(project);
    fs.writeFileSync(path.join(project, 'README.md'), 'Disposable context-transfer QA. Do not modify files.\n');
    execFileSync('git', ['init', '-q', project]);
    execFileSync('git', ['-C', project, 'add', 'README.md']);
    execFileSync('git', ['-C', project, '-c', 'user.name=Luna QA', '-c', 'user.email=qa@example.invalid', 'commit', '-qm', 'QA fixture']);
    const gitBefore = gitState(project);
    const codex = await configFor('codex'), claude = await configFor('claude-code');
    const parent = crypto.randomUUID(), spaceId = crypto.randomUUID();
    const markerA = `CONTEXT_A_${crypto.randomUUID().slice(0, 12)}`, markerB = `FINDING_B_${crypto.randomUUID().slice(0, 12)}`;
    const manifest = {parent, spaceId, markerA, markerB, codex, claude, project, evidence, gitBefore, pid, profileId: info.profileId};
    save(evidence, 'manifest', manifest);
    await call('Mutate', {op: 'createSpace', spaceId, deviceId: info.deviceId, path: project});
    await call('Mutate', {op: 'createChat', chatId: parent, spaceId, cwd: project, config: codex});
    await call('Mutate', {op: 'renameChat', chatId: parent, title: 'QA context roundtrip'});
    const first = await turn(parent, `Application QA. Do not use tools or edit files. Remember this code word: ${markerA}. Reply with that exact word only.`, 'parent-marker');
    assert.ok(first.some(reply => text(reply).includes(markerA)));
    const parentBinding = await identity(parent);
    save(evidence, 'parent-original-identity', parentBinding);
    const fork = await call('CreateSideChat', {parentId: parent, requestId: crypto.randomUUID()});
    assert.equal(fork.meta.contextMode, 'nativeFork');
    manifest.fork = fork.meta.id; save(evidence, 'manifest', manifest);
    save(evidence, 'native-fork', fork);
    assert.notEqual((await identity(fork.meta.id)).currentBinding.providerTaskId, parentBinding.currentBinding.providerTaskId);
    const findings = await turn(fork.meta.id, `Do not use tools. Remember this second QA code word: ${markerB}. Reply with both the original code word inherited from the source conversation and this second word.`, 'fork-findings');
    assert.ok(findings.some(reply => text(reply).includes(markerA) && text(reply).includes(markerB)));
    const holdingId = crypto.randomUUID();
    const hold = 'Application QA. Run exactly one harmless shell command: sleep 15. Do not read or modify files. After it finishes reply HOLD_COMPLETE.';
    save(evidence, 'hold-attempt', {parent, holdingId, hold});
    await call('QueueMessage', {chatId: parent, messageId: holdingId, text: hold, holdForTurnEnd: true});
    await until(async () => (await summary(parent)).phase === 'running', 'parent active');
    const merge = await transfer({kind: 'forkMerge', sourceTaskId: fork.meta.id, targetTaskId: parent, messageIds: findings.map(reply => reply.id)}, 'merge');
    assert.equal((await identity(parent)).currentBinding.providerTaskId, parentBinding.currentBinding.providerTaskId);
    assert.equal((await summary(parent)).phase, 'running', 'Applying findings must not interrupt the parent');
    const queuedId = crypto.randomUUID(), recall = 'Do not use tools. Repeat both distinct QA code words now available in this conversation, and nothing else.';
    await call('QueueMessage', {chatId: parent, messageId: queuedId, text: recall, followUpBehavior: 'steer', holdForTurnEnd: true});
    const held = await until(async () => (await call('WatchQueue', {chatId: parent})).items.find(item => item.id === queuedId && item.holdReason === 'steeringUnavailable'), 'pending context blocks steering');
    save(evidence, 'held-steer', held);
    assert.ok(!(await transcript(parent)).some(entry => entry.id === queuedId));
    await completed(parent, holdingId, hold);
    const sent = await call('SendQueuedMessageNow', {chatId: parent, id: queuedId, text: ''});
    assert.ok(sent.sent);
    const merged = await completed(parent, queuedId, recall);
    save(evidence, 'merge-recall', merged);
    assert.ok(merged.slice(merged.findIndex(entry => entry.id === queuedId) + 1).some(reply => reply.role === 'assistant' && text(reply).includes(markerA) && text(reply).includes(markerB)));
    assert.equal((await identity(parent)).currentBinding.providerTaskId, parentBinding.currentBinding.providerTaskId);
    assert.equal((await call('GetContextTransfer', {taskId: parent, transferId: merge.previewId})).receipt.status, 'consumed');
    let deltaRefusal;
    try {
      await call('PreviewContextTransfer', {kind: 'forkMerge', sourceTaskId: fork.meta.id, targetTaskId: parent, messageIds: findings.map(reply => reply.id)});
    } catch (error) { deltaRefusal = String(error); }
    assert.match(deltaRefusal ?? '', /no new visible findings/);
    save(evidence, 'merge-delta', {refusedDuplicate: true, reason: deltaRefusal, transferId: merge.previewId});
    for (const [name, config] of [['claude', claude], ['codex-return', codex]]) {
      const before = await identity(parent);
      const handoff = await transfer({kind: 'providerHandoff', sourceTaskId: parent, targetTaskId: parent, targetConfig: config}, name);
      assert.ok(!['running', 'starting'].includes((await summary(parent)).phase));
      const replies = await turn(parent, recall, `${name}-recall`);
      assert.ok(replies.some(reply => text(reply).includes(markerA) && text(reply).includes(markerB)), `${name} must recall both markers without either in the submitted prompt`);
      const after = await identity(parent); save(evidence, `${name}-identity`, after);
      assert.notEqual(after.currentBinding.providerTaskId, before.currentBinding.providerTaskId);
      assert.ok(after.currentBinding.generation > before.currentBinding.generation);
      assert.equal((await call('GetContextTransfer', {taskId: parent, transferId: handoff.previewId})).receipt.status, 'consumed');
    }
    await finishReadback(manifest);
  }
} catch (error) {
  if (evidence) save(evidence, `${mode}-failure`, {error: error.stack, pid, at: new Date().toISOString()});
  throw error;
} finally { ws.close(); }
