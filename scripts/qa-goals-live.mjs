#!/usr/bin/env node
// Coordinator-only live E2E. Uses the existing signed app and a fresh ordinary
// fixture repository; never launches LUNA, builds, creates worktrees or cleans data.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const [mode, directory] = process.argv.slice(2);
assert.ok(['run', 'snapshot'].includes(mode), 'usage: node scripts/qa-goals-live.mjs run|snapshot <absolute-evidence-directory>');
assert.ok(directory && path.isAbsolute(directory), 'use an absolute evidence directory outside disposable checkouts');
const tokenBudgetInput = process.env.LUNA_GOAL_QA_TOKEN_BUDGET ?? '60000';
assert.match(tokenBudgetInput, /^[1-9]\d*$/, 'LUNA_GOAL_QA_TOKEN_BUDGET must be a positive decimal integer');
const tokenBudget = Number(tokenBudgetInput);
assert.ok(Number.isSafeInteger(tokenBudget), 'LUNA_GOAL_QA_TOKEN_BUDGET must be a finite safe integer');
const evidence = path.resolve(directory);
const app = process.env.LUNA_GOAL_QA_APP ?? path.resolve('target/luna-macos-dev/LUNA Dev.app');
const port = process.env.LUNA_GOAL_QA_PORT ?? '59777';
assert.match(port, /^\d+$/);
const executable = fs.realpathSync(path.join(app, 'Contents/MacOS/luna'));
execFileSync('codesign', ['--verify', '--deep', '--strict', app]);
const pid = execFileSync('lsof', ['-t', '-nP', `-iTCP:${port}`, '-sTCP:LISTEN'], { encoding: 'utf8' }).trim();
assert.match(pid, /^\d+$/, 'one exact app listener is required');
assert.equal(execFileSync('ps', ['-p', pid, '-o', 'command='], { encoding: 'utf8' }).trim(), executable);
if (mode === 'run') {
  assert.ok(process.env.LUNA_GOAL_QA_MODEL, 'set LUNA_GOAL_QA_MODEL to an available Codex model');
  assert.ok(!fs.existsSync(evidence), 'use a fresh evidence directory for each run');
  fs.mkdirSync(evidence, { recursive: true });
}
const save = (name, value) => fs.writeFileSync(path.join(evidence, name), JSON.stringify(value, null, 2) + '\n');
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
let nextId = 1;
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, 30000);
    function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer); ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      else { if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true })); resolve(frame.item ?? frame.ok); }
    }
    ws.addEventListener('message', receive); ws.send(JSON.stringify({ id, method, params }));
  });
}
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function settle(receipt) {
  const deadline = Date.now() + 30000;
  while (receipt?.status === 'pending' && Date.now() < deadline) { await sleep(100); receipt = await call('GetTaskCommandReceipt', { commandId: receipt.commandId }); }
  return receipt;
}
async function mutate(params) {
  const result = await call('Mutate', params);
  if (result.receipt) assert.equal((await settle(result.receipt)).status, 'applied', JSON.stringify(result));
}
async function command(taskId, intent, label) {
  const snapshot = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
  const commandId = crypto.randomUUID();
  const params = { commandId, taskId, expectedTaskRevision: snapshot.taskRevision,
    expectedBindingGeneration: snapshot.bindingGeneration, intent };
  save(`${label}-command.json`, params);
  const receipt = await settle(await call('ExecuteTaskCommand', params));
  save(`${label}-receipt.json`, receipt);
  assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
  return receipt;
}
function entries(item) { return item?.frame?.reset ?? item?.reset ?? []; }
function text(item) { return entries(item).filter(e => e.role === 'assistant').flatMap(e => e.parts ?? []).filter(p => p.kind === 'text').map(p => p.text ?? '').join('\n'); }
async function observation(taskId) {
  const [messages, sessions, identity] = await Promise.all([
    call('WatchDocMessages', { chatId: taskId }), call('WatchSessions'), call('GetTaskIdentity', { taskId }),
  ]);
  return { at: new Date().toISOString(), messages, sessions: sessions.filter(s => s.chatId === taskId), identity };
}
async function waitFor(taskId, label, predicate, timeout = 180000) {
  const deadline = Date.now() + timeout;
  const timeline = [];
  while (Date.now() < deadline) {
    const read = await observation(taskId); timeline.push(read);
    if (predicate(read)) { save(`${label}-timeline.json`, timeline); return read; }
    const goal = read.messages.contextControl?.goal?.goal;
    if (goal?.status === 'budgetLimited') {
      save(`${label}-timeline.json`, timeline);
      save(`${label}-budget-limited.json`, { classification: 'qa-token-budget-exhausted', taskId,
        tokenBudget: goal.tokenBudget, tokensUsed: goal.tokensUsed, timeUsedSeconds: goal.timeUsedSeconds, at: read.at });
      const error = new Error(`${label}: goal reported budgetLimited (${goal.tokensUsed} used / ${goal.tokenBudget} allowed); rerun with a suitable finite LUNA_GOAL_QA_TOKEN_BUDGET`);
      error.code = 'LUNA_GOAL_QA_BUDGET_LIMITED';
      throw error;
    }
    await sleep(500);
  }
  save(`${label}-timeline.json`, timeline);
  throw new Error(`${label} did not reach its expected state; retained timeline explains the failure`);
}
try {
  if (mode === 'snapshot') {
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json')));
    await command(manifest.taskId, { kind: 'goal', operation: { action: 'get' } }, `reopen-${Date.now()}`);
    save(`reopen-${Date.now()}-readback.json`, await observation(manifest.taskId));
  } else {
    const model = process.env.LUNA_GOAL_QA_MODEL;
    const reasoning = process.env.LUNA_GOAL_QA_REASONING ?? 'high';
    const models = await call('ListModels', { harness: 'codex', refresh: true });
    assert.ok(models.some(item => item.id === model), 'requested model must be present in the real catalog');
    const info = await call('EngineInfo');
    const workspace = path.join(evidence, 'workspace'); fs.mkdirSync(workspace);
    const gitEnv = { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
      GIT_AUTHOR_NAME: 'LUNA QA', GIT_AUTHOR_EMAIL: 'qa@example.invalid', GIT_COMMITTER_NAME: 'LUNA QA', GIT_COMMITTER_EMAIL: 'qa@example.invalid' };
    const git = args => execFileSync('git', ['-C', workspace, ...args], { env: gitEnv, encoding: 'utf8' });
    git(['init', '-q', '-b', 'main']); fs.writeFileSync(path.join(workspace, 'README.md'), 'Disposable LUNA goal-control E2E fixture.\n');
    git(['add', 'README.md']); git(['commit', '-q', '-m', 'Goal QA fixture']);
    const spaceId = crypto.randomUUID(), taskId = crypto.randomUUID(), marker = `LUNA_GOAL_${crypto.randomBytes(5).toString('hex')}`;
    const config = { harness: 'codex', model, reasoning, sandbox: 'read-only', permissionMode: 'ask' };
    if (process.env.LUNA_GOAL_QA_ACCOUNT) config.providerAccountId = process.env.LUNA_GOAL_QA_ACCOUNT;
    save('manifest.json', { taskId, spaceId, workspace, marker, model, reasoning, tokenBudget, app, executable, port, pid });
    await mutate({ op: 'createSpace', spaceId, deviceId: info.deviceId, path: workspace, name: `Goal QA ${marker}`, gitDetected: true });
    await mutate({ op: 'createChat', chatId: taskId, spaceId, cwd: workspace, config });
    await mutate({ op: 'renameChat', chatId: taskId, title: `Goal QA ${marker}` });
    await command(taskId, { kind: 'session', command: { kind: 'run', messageId: crypto.randomUUID(),
      request: { prompt: `Reply exactly ${marker}_BOOT. Use no tools. Do not create a goal.`, harness: 'codex', model, reasoning,
        cwd: workspace, sandbox: 'read-only', permissionMode: 'ask', resume: null } } }, 'bootstrap');
    const idle = await waitFor(taskId, 'bootstrap', read => text(read.messages).includes(`${marker}_BOOT`)
      && !read.sessions.some(s => ['working', 'awaitingInput', 'awaitingApproval'].includes(s.status)));
    await command(taskId, { kind: 'goal', operation: { action: 'get' } }, 'ordinary-prompt-get');
    assert.equal((await observation(taskId)).messages.contextControl.goal.goal, null, 'ordinary work must not infer a goal');
    const checkpointsBefore = await call('ListGitCheckpoints', { chatId: taskId, cwd: workspace }); save('checkpoints-before.json', checkpointsBefore);
    const objective = `Use only text and native goal tools. Read this thread history. If ${marker}_STEP1 is absent, reply exactly ${marker}_STEP1 and leave the goal active for another idle continuation. If STEP1 is present, reply ${marker}_STEP2 and call update_goal with status complete because both requested steps are achieved. Do not create another goal. Do not call external tools or ask questions.`;
    await command(taskId, { kind: 'goal', operation: { action: 'set', objective, status: 'paused', tokenBudget: { mode: 'limited', tokens: tokenBudget } } }, 'create-paused');
    const created = (await observation(taskId)).messages.contextControl.goal.goal;
    assert.equal(created.status, 'paused'); assert.equal(created.tokenBudget, tokenBudget);
    await command(taskId, { kind: 'goal', operation: { action: 'set', objective } }, 'update-preserve-budget');
    let goal = (await observation(taskId)).messages.contextControl.goal.goal;
    assert.equal(goal.tokenBudget, tokenBudget); assert.equal(goal.createdAt, created.createdAt); assert.equal(goal.status, 'paused');
    await command(taskId, { kind: 'goal', operation: { action: 'set', tokenBudget: { mode: 'unlimited' } } }, 'remove-budget');
    assert.equal((await observation(taskId)).messages.contextControl.goal.goal.tokenBudget, null);
    await command(taskId, { kind: 'goal', operation: { action: 'set', tokenBudget: { mode: 'limited', tokens: tokenBudget } } }, 'restore-budget');
    await command(taskId, { kind: 'goal', operation: { action: 'set', status: 'active' } }, 'resume');
    const completed = await waitFor(taskId, 'idle-continuation', read => read.messages.contextControl?.goal?.goal?.status === 'complete'
      && text(read.messages).includes(`${marker}_STEP2`) && !read.sessions.some(s => s.status === 'working'));
    assert.ok(text(completed.messages).includes(`${marker}_STEP1`), 'first continuation must remain visible');
    const userRows = entries(completed.messages).filter(e => e.role === 'user');
    assert.equal(userRows.length, entries(idle.messages).filter(e => e.role === 'user').length, 'goal continuation must not synthesize user messages');
    const steps = entries(completed.messages).filter(e => e.role === 'assistant').filter(e => (e.parts ?? []).some(p => p.kind === 'text' && /_STEP[12]/.test(p.text ?? '')));
    assert.ok(steps.length >= 2, 'continuations require distinct transcript segments');
    await command(taskId, { kind: 'goal', operation: { action: 'get' } }, 'complete-readback');
    goal = (await observation(taskId)).messages.contextControl.goal.goal;
    assert.ok(goal.tokensUsed > 0); assert.ok(goal.timeUsedSeconds >= 0); assert.equal(goal.status, 'complete');
    const checkpointsAfter = await call('ListGitCheckpoints', { chatId: taskId, cwd: workspace }); save('checkpoints-after.json', checkpointsAfter);
    const checkpointRows = Array.isArray(checkpointsAfter) ? checkpointsAfter : checkpointsAfter.checkpoints;
    const priorRows = Array.isArray(checkpointsBefore) ? checkpointsBefore : checkpointsBefore.checkpoints;
    assert.ok(checkpointRows.length >= priorRows.length + 2, 'each autonomous continuation must retain a history checkpoint');
    await command(taskId, { kind: 'goal', operation: { action: 'set', status: 'paused' } }, 'explicit-pause');
    assert.equal((await observation(taskId)).messages.contextControl.goal.goal.status, 'paused');
    await command(taskId, { kind: 'goal', operation: { action: 'set', status: 'complete' } }, 'explicit-complete');
    save('final-readback.json', await observation(taskId));
    save('result.json', { passed: true, taskId, marker, model, reasoning, tokenBudget, checks: ['ordinary-no-goal', 'create-pause', 'update-preserves-status-budget', 'remove-budget', 'idle-two-turn-continuation', 'distinct-history-checkpoints', 'provider-usage', 'explicit-complete'] });
    console.log(JSON.stringify({ passed: true, taskId, evidence }));
  }
} catch (error) {
  save(`failure-${Date.now()}.json`, { error: String(error), code: error.code, stack: error.stack });
  throw error;
} finally { ws.close(); }
