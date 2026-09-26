#!/usr/bin/env node
// Read-only half of the native first-submit E2E. The UI actions and screenshots
// are specified in docs/qa/project-first-submit-recovery-e2e.md.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const [mode, evidenceArg, screenshotArg] = process.argv.slice(2);
assert.ok(['baseline', 'assert-setup', 'assert-precreate', 'assert-empty'].includes(mode),
  'mode must be baseline, assert-setup, assert-precreate, or assert-empty');
assert.ok(evidenceArg && path.isAbsolute(evidenceArg), 'supply an absolute evidence directory');
const evidence = path.resolve(evidenceArg);
assert.ok(evidence.startsWith(`${root}/target/qa/`), 'evidence must stay below this checkout/target/qa');
fs.mkdirSync(evidence, { recursive: true });

const identities = {
  defaults: ['LUNA QA Defaults', 'com.novnski.luna.qa.defaults', '59788', 'defaults'],
  'projects-integration': ['LUNA QA Projects Integration',
    'com.novnski.luna.qa.projects-integration', '59791', 'projects-integration'],
};
const identityName = process.env.LUNA_PROJECT_DEFAULTS_QA_IDENTITY || 'defaults';
assert.ok(Object.hasOwn(identities, identityName), 'unknown QA identity');
const [appName, bundleId, ipcPort, dataName] = identities[identityName];
const dataDir = path.join(root, 'target/luna-qa-data', dataName);
for (const [key, expected] of Object.entries({
  LUNA_DEV_APP_NAME: appName,
  LUNA_DEV_BUNDLE_ID: bundleId,
  LUNA_DEV_IPC_PORT: ipcPort,
  LUNA_DEV_DATA_DIR: dataDir,
})) assert.equal(process.env[key], expected, `exact QA override ${key} is required`);
assert.equal(process.platform, 'darwin', 'native macOS QA only');
const app = path.join(root, 'target/luna-macos-dev', `${appName}.app`);
const executable = fs.realpathSync(path.join(app, 'Contents/MacOS/luna'));
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-',
  path.join(app, 'Contents/Info.plist')], { encoding: 'utf8' }));
assert.equal(plist.CFBundleIdentifier, bundleId);
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, ipcPort);
assert.equal(path.resolve(plist.LSEnvironment.LUNA_DATA_DIR), dataDir);
execFileSync('codesign', ['--verify', '--deep', '--strict', app]);
const listener = execFileSync('lsof', ['-nP', `-iTCP:${ipcPort}`, '-sTCP:LISTEN', '-Fp'],
  { encoding: 'utf8' }).match(/^p[0-9]+$/gm) ?? [];
assert.equal(listener.length, 1, 'exactly one QA listener required');
const pid = Number(listener[0].slice(1));
assert.equal(execFileSync('ps', ['-ww', '-p', String(pid), '-o', 'command='],
  { encoding: 'utf8' }).trim(), executable);

const projectId = 'qa-defaults-a';
const prompt = 'Reply with exactly PROJECT_DEFAULTS_NATIVE_OK. Do not use tools or change files.';
const setupCommand = 'printf \'SETUP_CWD=%s\\n\' "$PWD"\n'
  + 'if [ ! -f .qa-setup-allow ]; then\n'
  + '  printf \'PROJECT_SETUP_BLOCKED\\n\'\n'
  + '  exit 7\nfi\nprintf \'PROJECT_SETUP_READY\\n\'';

const ws = new WebSocket(`ws://127.0.0.1:${ipcPort}`);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${method} timed out`)), 15000);
    const receive = event => {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer);
      ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(frame.err));
      else {
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
        resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    };
    ws.addEventListener('message', receive);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
function worktrees(repo) {
  return execFileSync('git', ['-C', repo, 'worktree', 'list', '--porcelain'],
    { encoding: 'utf8' }).split('\n').filter(line => line.startsWith('worktree '))
    .map(line => line.slice('worktree '.length)).sort();
}
function write(name, value) {
  const file = path.join(evidence, name);
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
  return file;
}
const spaces = await call('WatchSpaces');
const project = spaces.find(space => space.id === projectId);
assert.ok(project && project.gitDetected, 'synthetic Git project A is required');
const commands = await call('GetProjectCommands', { projectId });
const setup = commands.filter(action => action.setupOnWorktreeCreate);
assert.equal(setup.length, 1, 'exactly one setup action required');
assert.equal(setup[0].command, setupCommand);
assert.equal(setup[0].waitForSetup, true);
const chats = await call('WatchChats');
const sessions = await call('WatchSessions');
const treePaths = worktrees(project.path);
const identity = { app, executable, pid, ipcPort, dataDir, projectId };

try {
  if (mode === 'baseline') {
    const file = write('baseline.json', { identity, prompt, project,
      commands, chatIds: chats.map(chat => chat.id).sort(),
      sessionChatIds: sessions.map(session => session.chatId).sort(),
      worktrees: treePaths, capturedAt: new Date().toISOString() });
    console.log(JSON.stringify({ result: 'baseline', artifact: file }));
    process.exitCode = 0;
  } else {
    const baseline = JSON.parse(fs.readFileSync(path.join(evidence, 'baseline.json'), 'utf8'));
    assert.deepEqual(baseline.identity, identity, 'QA app identity changed since baseline');
    assert.equal(baseline.project.id, projectId);
    const failures = [];
    const check = (condition, message) => { if (!condition) failures.push(message); };
    const fresh = chats.filter(chat => !baseline.chatIds.includes(chat.id));
    const freshTrees = treePaths.filter(tree => !baseline.worktrees.includes(tree));
    const screenshot = screenshotArg ? path.resolve(screenshotArg) : null;
    check(screenshot && fs.existsSync(screenshot) && fs.statSync(screenshot).size > 1000,
      'pass a saved native UI screenshot showing the relevant controls');
    const observed = { identity, mode, prompt, screenshot, freshChats: fresh,
      freshWorktrees: freshTrees, failures, capturedAt: new Date().toISOString() };
    if (mode === 'assert-setup') {
      check(fresh.length === 1 && fresh[0].spaceId === projectId,
        'failed setup must retain exactly one visible A task');
      check(freshTrees.length === 1, 'failed setup must retain one materialized worktree');
      if (fresh.length === 1) {
        const task = fresh[0];
        check(task.cwd && freshTrees.includes(task.cwd),
          'retained task must point to its new worktree');
        check(task.config?.harness === 'codex' && task.config?.model === 'gpt-6-luna',
          'retained task config must be Codex GPT-6 Luna');
        check(!task.harnessSessionId,
          'setup failure must not create a provider session ID');
        const reads = await Promise.allSettled([
          call('ProjectCommandStatus', { chatId: task.id }),
          call('GetTaskCommandSnapshot', { taskId: task.id, afterTaskSequence: 0, limit: 50 }),
          call('GetComposerDraft', { taskId: task.id }),
        ]);
        observed.reads = reads.map(read => read.status === 'fulfilled'
          ? { ok: read.value } : { error: String(read.reason) });
        check(reads.every(read => read.status === 'fulfilled'),
          'failed setup receipt, task command events and recoverable draft must remain readable');
        if (reads.every(read => read.status === 'fulfilled')) {
          const [status, commandsRead, draftRead] = reads.map(read => read.value);
          const failed = status.runs.filter(run => run.setup && run.chatId === task.id
            && run.status === 'failed' && run.exitCode === 7 && run.attempt === 1);
          check(failed.length === 1 && failed[0].cwd === task.cwd,
            'Terminal → Commands must expose one failed setup run in the task checkout');
          const runReceipts = commandsRead.events.map(event => event.receipt)
            .filter(receipt => receipt.intent === 'run');
          check(runReceipts.length === 1 && runReceipts[0].status === 'rejected',
            'exactly one rejected Run receipt expected before provider start');
          check(runReceipts[0]?.recoveryAction ===
            'Worktree setup failed. Open Terminal → Commands to inspect output and retry setup, then retry the task.',
          'the exact actionable recovery text must be preserved');
          check(!commandsRead.events.some(event => event.receipt.intent === 'delete'),
            'setup rejection must not issue DeleteCommand');
          check(draftRead.submissions.some(submission => submission.taskId === task.id
            && submission.state === 'recoverable' && submission.snapshot.text === prompt),
          'the original prompt must remain recoverable on the same task');
          check(!sessions.some(session => session.chatId === task.id
            && ['working', 'awaitingApproval', 'awaitingInput'].includes(session.status)),
          'provider must not be active before setup succeeds');
        }
      }
    } else {
      check(fresh.length === 0, `${mode} must leave no new task`);
      check(freshTrees.length === 0, `${mode} must leave no new worktree`);
      check(!sessions.some(session => !baseline.sessionChatIds.includes(session.chatId)),
        `${mode} must not start a provider session`);
      const canvas = await call('GetComposerDraft', {});
      observed.canvas = canvas;
      check(canvas.draft?.text === prompt || canvas.submissions?.some(
        submission => submission.snapshot.text === prompt),
      `${mode} must preserve the unsent prompt on the canvas`);
      if (mode === 'assert-empty') {
        const commandId = process.env.LUNA_QA_REJECTED_RUN_COMMAND_ID;
        check(Boolean(commandId), 'empty/no-setup control requires rejected Run command ID');
        if (commandId) {
          const receipt = await call('GetTaskCommandReceipt', { commandId });
          observed.rejectedRun = receipt;
          check(receipt?.status === 'rejected' && receipt?.intent === 'run',
            'empty/no-setup control must have a rejected Run receipt');
        }
      }
    }
    const file = write(`${mode}.json`, observed);
    console.log(JSON.stringify({ result: failures.length ? 'FAIL' : 'PASS',
      failures, artifact: file }));
    if (failures.length) process.exitCode = 1;
  }
} finally {
  ws.close();
}
