#!/usr/bin/env node
// Default: local fixture preparation only. Native modes need the coordinator's
// desktop grant. They never build or send a PR-create RPC.
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync, readFileSync, existsSync, realpathSync, statSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';

const project = realpathSync(resolve(import.meta.dirname, '..'));
const root = join(project, 'target/qa/github-pr-native');
const checkout = join(root, 'checkout');
const fixture = realpathSync(join(project, 'scripts/fixtures/github-pr'));
const dataRoot = join(project, 'target/luna-qa-data/pullrequests');
const app = join(project, 'target/luna-macos-dev/LUNA QA Pull Requests.app');
const executable = join(app, 'Contents/MacOS/luna');
const bundleId = 'com.novnski.luna.qa.pullrequests';
const port = 59790;
const args = process.argv.slice(2);
assert.ok(args.length <= 1 && args.every(a => ['--seed', '--restart-and-seed', '--launch-and-seed'].includes(a)),
  'Usage: prepare-github-pr-qa.mjs [--seed|--restart-and-seed|--launch-and-seed]');
const native = args.length > 0;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
mkdirSync(checkout, { recursive: true });
assert.equal(realpathSync(checkout), checkout, 'Fixture must stay inside this worktree');
const git = (...args) => execFileSync('git', args, {
  cwd: checkout, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null' },
}).trim();
if (!existsSync(join(checkout, '.git'))) {
  git('init', '-q', '-b', 'main');
  git('config', 'core.hooksPath', join(root, 'no-hooks'));
  writeFileSync(join(checkout, 'README.md'), '# Draft pull request fixture\n');
  git('add', 'README.md');
  git('-c', 'user.name=QA', '-c', 'user.email=qa@example.invalid', 'commit', '-qm', 'Initial fixture');
  git('checkout', '-qb', 'feature/pr');
  writeFileSync(join(checkout, 'README.md'), '# Draft pull request fixture\n\nA change ready for review.\n');
  git('add', 'README.md');
  git('-c', 'user.name=QA', '-c', 'user.email=qa@example.invalid', 'commit', '-qm', 'Document draft pull request workflow');
  git('remote', 'add', 'origin', 'git@github.com:acme/luna.git');
  git('update-ref', 'refs/remotes/origin/feature/pr', git('rev-parse', 'HEAD'));
  git('update-ref', 'refs/remotes/origin/main', git('rev-parse', 'main'));
  git('symbolic-ref', 'refs/remotes/origin/HEAD', 'refs/remotes/origin/main');
  git('config', 'branch.feature/pr.remote', 'origin');
  git('config', 'branch.feature/pr.merge', 'refs/heads/feature/pr');
}
assert.equal(git('remote', 'get-url', 'origin'), 'git@github.com:acme/luna.git');
assert.equal(git('branch', '--show-current'), 'feature/pr');
writeFileSync(join(root, 'sha'), git('rev-parse', 'HEAD'));
if (!existsSync(join(root, 'mode'))) writeFileSync(join(root, 'mode'), 'success');
const launchEnv = {
  LUNA_PR_FIXTURE: root, LUNA_NO_LOGIN_SHELL: '1', PATH: `${fixture}:${process.env.PATH}`,
  LUNA_DATA_DIR: dataRoot, LUNA_IPC_PORT: String(port),
};
writeFileSync(join(root, 'launch-env.json'), JSON.stringify(launchEnv, null, 2));
if (!native) {
  console.log(JSON.stringify({ root, checkout, launchEnvironment: join(root, 'launch-env.json'), nativeStarted: false }));
  process.exit(0);
}

function listener(listenPort) {
  const result = spawnSync('lsof', ['-t', '-nP', `-iTCP:${listenPort}`, '-sTCP:LISTEN'], { encoding: 'utf8' });
  if (result.status === 1 && !result.stdout.trim()) return null;
  assert.equal(result.status, 0, `Could not inspect IPC ${listenPort}`);
  const pids = [...new Set(result.stdout.trim().split(/\s+/))];
  assert.equal(pids.length, 1, `Expected one listener on ${listenPort}`);
  assert.match(pids[0], /^\d+$/);
  return Number(pids[0]);
}
function command(pid) {
  const result = spawnSync('ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
  return result.status === 0 ? result.stdout.trim() : null;
}
function qaProcesses() {
  const result = execFileSync('ps', ['-axo', 'pid=,command='], { encoding: 'utf8' });
  return result.split('\n').flatMap(line => {
    const match = line.trim().match(/^(\d+)\s+(.+)$/);
    return match && match[2] === executable ? [Number(match[1])] : [];
  });
}
function fixtureLines(name) {
  return existsSync(join(root, name))
    ? readFileSync(join(root, name), 'utf8').trim().split('\n').filter(Boolean)
    : [];
}
function mainUnchanged(expected, phase) {
  assert.equal(listener(59777), expected, `Main IPC changed ${phase}`);
}
function verifiedListener() {
  const pid = listener(port);
  assert.ok(pid, 'QA IPC is not listening');
  assert.equal(command(pid), executable, 'IPC must belong to this exact QA executable');
  assert.deepEqual(qaProcesses(), [pid], 'Expected only one exact QA bundle process');
  return pid;
}
function verifyBundle() {
  assert.equal(realpathSync(dataRoot), dataRoot, 'QA data root must be canonical and worktree-local');
  const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', join(app, 'Contents/Info.plist')], { encoding: 'utf8' }));
  assert.equal(plist.CFBundleIdentifier, bundleId);
  assert.equal(plist.LSEnvironment?.LUNA_DATA_DIR, dataRoot);
  assert.equal(String(plist.LSEnvironment?.LUNA_IPC_PORT), String(port));
  execFileSync('codesign', ['--verify', '--deep', '--strict', app], { stdio: 'pipe' });
}
async function connect() {
  const ws = new WebSocket(`ws://127.0.0.1:${port}`);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.close(); reject(new Error('QA IPC connect timeout')); }, 5000);
    ws.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
    ws.addEventListener('error', error => { clearTimeout(timer); reject(error); }, { once: true });
  });
  let sequence = 0;
  const call = async (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timeout`)); }, 75000);
    const receive = event => {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer); ws.removeEventListener('message', receive);
      frame.err ? reject(new Error(`${method}: ${JSON.stringify(frame.err)}`)) : resolve(frame.ok);
    };
    ws.addEventListener('message', receive); ws.send(JSON.stringify({ id, method, params }));
  });
  return { call, close: () => ws.close() };
}
async function ownership(client, expectedPid) {
  assert.equal(verifiedListener(), expectedPid, 'QA listener changed during preflight');
  const expectedClaim = createHash('sha256').update('LUNA local root v1\0').update(realpathSync(dataRoot)).digest('hex');
  const info = await client.call('EngineInfo');
  assert.ok(info.capabilities.includes('luna-owner-ipc-v1'));
  assert.equal(info.deviceId, readFileSync(join(dataRoot, 'device-id'), 'utf8').trim());
  assert.equal((await client.call('LocalOwnerRootClaim')).claim, expectedClaim,
    'LocalOwnerRootClaim did not prove this exact QA data root');
  assert.equal(verifiedListener(), expectedPid);
  return { deviceId: info.deviceId, rootClaim: expectedClaim };
}
const mainPid = listener(59777);
verifyBundle();
const mode = args[0];
const launch = mode === '--restart-and-seed' || mode === '--launch-and-seed';
let previousPid = null;
if (mode === '--restart-and-seed') {
  previousPid = verifiedListener();
  const oldClient = await connect();
  try { await ownership(oldClient, previousPid); } finally { oldClient.close(); }
  // NSRunningApplication targets a verified PID and exact executable URL.
  execFileSync('osascript', ['-l', 'JavaScript', '-e', `ObjC.import("AppKit"); function run(argv) {
    const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
    if (app.isNil() || ObjC.unwrap(app.executableURL.path) !== argv[1]) throw new Error("QA identity changed");
    app.terminate;
  }`, String(previousPid), executable], { stdio: 'pipe' });
  const quitDeadline = Date.now() + 45000;
  while (Date.now() < quitDeadline && command(previousPid) === executable) await pause(250);
  assert.notEqual(command(previousPid), executable, 'QA app refused graceful quit; left intact, no forced termination');
  assert.deepEqual(qaProcesses(), [], 'An existing QA process would ignore the launch environment');
  assert.equal(listener(port), null, 'QA port is occupied after graceful quit');
  mainUnchanged(mainPid, 'during QA restart');
} else if (mode === '--launch-and-seed') {
  assert.equal(listener(port), null, 'QA port is already occupied; stopped launch refused');
  assert.deepEqual(qaProcesses(), [], 'QA executable is unexpectedly live without its listener; stopped launch refused');
  assert.equal(fixtureLines('writes.jsonl').length, 0, 'Fixture has a PR POST before initial Create; stopped launch refused');
}
if (launch) {
  mainUnchanged(mainPid, 'before QA launch');
  const fakeGh = join(fixture, 'gh');
  assert.ok(existsSync(fakeGh) && statSync(fakeGh).isFile() && (statSync(fakeGh).mode & 0o111), 'Fake gh is missing or not executable');
  assert.equal(launchEnv.LUNA_PR_FIXTURE, root);
  assert.equal(launchEnv.LUNA_NO_LOGIN_SHELL, '1');
  assert.ok(launchEnv.PATH.startsWith(`${fixture}:`), 'Fake gh is not first in launch PATH');
  assert.equal(launchEnv.LUNA_DATA_DIR, dataRoot);
  assert.equal(launchEnv.LUNA_IPC_PORT, String(port));
  const envArgs = Object.entries(launchEnv).flatMap(([key, value]) => ['--env', `${key}=${value}`]);
  execFileSync('open', ['-g', '-n', ...envArgs, app], { stdio: 'pipe' });
  const launchDeadline = Date.now() + 45000;
  while (Date.now() < launchDeadline && !listener(port)) await pause(250);
  mainUnchanged(mainPid, 'during QA launch');
}
const pid = verifiedListener();
if (previousPid !== null) assert.notEqual(pid, previousPid, 'Fixture environment requires a fresh QA process');
const client = await connect();
try {
  const owner = await ownership(client, pid); // before any local seed mutation
  mainUnchanged(mainPid, 'before QA seed');
  if (mode === '--launch-and-seed') {
    assert.equal(fixtureLines('writes.jsonl').length, 0, 'Fixture POST appeared before QA seed');
  }
  const mutate = async params => {
    const result = await client.call('Mutate', params);
    let receipt = result.receipt;
    const deadline = Date.now() + 15000;
    while (receipt?.status === 'pending' && Date.now() < deadline) {
      await pause(50);
      receipt = await client.call('ReadTaskCommandReceipt', { taskId: params.chatId, commandId: receipt.commandId });
    }
    if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
  };
  const spaceId = 'github-pr-qa-space', chatId = 'github-pr-qa-chat';
  await mutate({ op: 'createSpace', spaceId, deviceId: owner.deviceId, path: checkout, name: 'Pull Request QA', gitDetected: true });
  await mutate({ op: 'createChat', chatId, spaceId, deviceId: owner.deviceId, cwd: checkout, branch: 'feature/pr', config: { harness: 'codex', model: 'gpt-6-luna', sandbox: 'workspace-write' } });
  await mutate({ op: 'renameChat', chatId, title: 'Draft pull request acceptance' });
  const beforeCalls = fixtureLines('calls.jsonl').length;
  const beforeWrites = fixtureLines('writes.jsonl').length;
  if (mode === '--launch-and-seed') {
    assert.equal(beforeWrites, 0, 'Fixture POST appeared before initial preview');
  }
  const preview = await client.call('GetGitHubPrPreview', { cwd: checkout });
  assert.equal(preview.repository, 'github.com/acme/luna');
  assert.equal(preview.checkout.cwd, checkout);
  assert.equal(preview.head, 'feature/pr');
  assert.equal(preview.checkout.head, git('rev-parse', 'HEAD'));
  const freshCalls = fixtureLines('calls.jsonl').slice(beforeCalls).map(line => JSON.parse(line));
  assert.ok(freshCalls.length >= 2, 'Native preview did not append fresh fake-gh calls');
  assert.ok(freshCalls.every(call => call.cwd === checkout), 'Fixture call used a different checkout');
  assert.ok(freshCalls.some(call => call.args.includes('repos/acme/luna')), 'Missing fixture repository read');
  assert.ok(freshCalls.some(call => call.args.includes('repos/acme/luna/branches/feature%2Fpr')), 'Missing exact fixture head read');
  assert.ok(freshCalls.every(call => !call.args.includes('POST')), 'Preflight must be read-only at GitHub boundary');
  assert.equal(fixtureLines('writes.jsonl').length, beforeWrites, 'A preflight must not create a PR');
  await ownership(client, pid);
  verifyBundle();
  mainUnchanged(mainPid, 'during QA preflight');
  const proof = { passed: true, at: new Date().toISOString(), pid, previousPid, executable, bundleId, dataRoot,
    ...owner, mainPid, spaceId, chatId, checkout, operationId: preview.operationId,
    callCountBefore: beforeCalls, callCountAfter: fixtureLines('calls.jsonl').length, freshCalls,
    fixturePostsBefore: beforeWrites, fixturePostsAfter: fixtureLines('writes.jsonl').length, hostedWrites: 0 };
  writeFileSync(join(root, 'native-preflight.json'), JSON.stringify(proof, null, 2));
  console.log(JSON.stringify({ passed: true, pid, spaceId, chatId, proof: join(root, 'native-preflight.json'), fixtureReadCount: freshCalls.length, hostedWrites: 0 }));
} finally { client.close(); }
