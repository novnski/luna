#!/usr/bin/env node
// Real-app IPC acceptance. Creates only disposable Git roots and QA app records.
// Failure modes: default-stack diff abort, stale save overwrites, upload collision,
// checkpoint readback loss, and a shared checkout incorrectly allowing restore.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = fs.realpathSync(path.resolve(import.meta.dirname, '..'));
const identityKeys = ['LUNA_DEV_APP_NAME', 'LUNA_DEV_BUNDLE_ID', 'LUNA_DEV_IPC_PORT', 'LUNA_DEV_DATA_DIR'];
const suppliedIdentity = identityKeys.filter(key => Object.hasOwn(process.env, key));
assert.ok(suppliedIdentity.length === 0 || suppliedIdentity.length === 4, 'QA identity overrides require all four LUNA_DEV_* values');
const overridden = suppliedIdentity.length === 4;
for (const key of suppliedIdentity) assert.match(process.env[key], /^\S(?:[^\r\n]*\S)?$/, `${key} must be nonempty and single-line`);
const appName = overridden ? process.env.LUNA_DEV_APP_NAME : 'LUNA QA Workspace';
const bundleId = overridden ? process.env.LUNA_DEV_BUNDLE_ID : 'com.novnski.luna.qa.workspace';
const portValue = overridden ? process.env.LUNA_DEV_IPC_PORT : process.env.LUNA_IPC_PORT ?? '59783';
assert.match(appName, /^LUNA QA [A-Za-z0-9][A-Za-z0-9 -]{0,48}$/);
assert.match(bundleId, /^com\.novnski\.luna\.qa\.[a-z][a-z0-9-]{0,47}$/);
assert.match(portValue, /^[1-9][0-9]{0,4}$/);
const port = Number(portValue);
assert.ok(port >= 1024 && port <= 65535 && port !== 59777, 'A separate QA IPC port is required');
if (!overridden) assert.equal(port, 59783, 'A different QA port requires all four LUNA_DEV_* identity values');
const dataDir = overridden ? process.env.LUNA_DEV_DATA_DIR : path.join(root, 'target/luna-qa-data/workspace');
assert.ok(path.isAbsolute(dataDir));
const bundle = path.join(root, 'target/luna-macos-dev', `${appName}.app`);
const target = path.join(root, 'target');
assert.equal(fs.realpathSync(target), target, 'QA target must not redirect through symlinks');
const dataRoot = fs.realpathSync(dataDir);
assert.equal(dataRoot, dataDir, 'QA data path must be canonical');
assert.ok(dataRoot.startsWith(`${target}${path.sep}`), 'QA data must be inside this checkout target');
const staging = path.join(target, 'luna-macos-dev');
assert.ok(dataRoot !== staging && !dataRoot.startsWith(`${staging}${path.sep}`), 'QA data must be outside bundle staging and primary data');
assert.equal(fs.realpathSync(bundle), bundle, 'QA bundle must not redirect through symlinks');
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const appExecutable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], { encoding: 'utf8' }));
assert.equal(plist.CFBundleIdentifier, bundleId);
assert.equal(plist.CFBundleDisplayName, appName);
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, dataRoot);
assert.equal(plist.LSEnvironment.LUNA_QA_MCP_ISOLATION, '1', 'QA requires signed MCP isolation');
const pids = execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], { encoding: 'utf8' }).match(/^p\d+$/gm) ?? [];
assert.equal(new Set(pids).size, 1, 'Exactly one isolated QA app must be listening');
const appPid = pids[0].slice(1);
assert.equal(execFileSync('ps', ['-p', appPid, '-o', 'comm='], { encoding: 'utf8' }).trim(), appExecutable);
const report = path.resolve(process.env.LUNA_NATIVE_REPORT_DIR || path.join(root, 'target/qa/workspace-native-' + Date.now()));
assert.ok(report.startsWith(path.join(root, 'target/qa/')));
fs.mkdirSync(report, { recursive: true });
const fixture = fs.realpathSync(fs.mkdtempSync(path.join(report, 'fixture-')));
const repo = path.join(fixture, 'repo');
fs.mkdirSync(repo);
const chatId = crypto.randomUUID();
const spaceId = crypto.randomUUID();
const uploadId = crypto.randomBytes(4).toString('hex');
const turnId = crypto.randomBytes(24).toString('hex');
const blobPaths = new Set();
const checks = [];
let projectCreated = false;
let chatCreated = false;
let failure;
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
let nextId = 1;
function git(args, env = {}) {
  return execFileSync('git', ['-C', repo, ...args], { encoding: 'utf8', env: {
    ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_AUTHOR_NAME: 'LUNA QA', GIT_AUTHOR_EMAIL: 'qa@example.invalid',
    GIT_COMMITTER_NAME: 'LUNA QA', GIT_COMMITTER_EMAIL: 'qa@example.invalid', ...env,
  }}).trim();
}
function evidence(name, value) {
  fs.appendFileSync(path.join(report, 'events.jsonl'), JSON.stringify({ at: new Date().toISOString(), name, value }) + '\n');
}
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', listener); reject(new Error(`${method} timeout`)); }, 15000);
    function listener(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer);
      ws.removeEventListener('message', listener);
      evidence(method, frame);
      if (frame.err) reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      else resolve(frame.ok);
    }
    ws.addEventListener('message', listener);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function mutate(params) {
  const result = await call('Mutate', params);
  let receipt = result.receipt;
  const deadline = Date.now() + 15000;
  while (receipt?.status === 'pending' && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 50));
    receipt = await call('GetTaskCommandReceipt', { commandId: receipt.commandId });
  }
  if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
  return result;
}
async function check(name, fn) {
  await fn();
  checks.push(name);
  evidence('passed', name);
  console.log(`PASS ${name}`);
}
function seedCheckpoint(phase) {
  // Fixture-only refs exercise native readback without starting an agent turn.
  const indexTree = git(['write-tree']);
  const env = { GIT_INDEX_FILE: path.join(repo, '.git', 'qa-private-index') };
  git(['read-tree', indexTree], env);
  git(['add', '-A', '--', '.'], env);
  const workTree = git(['write-tree'], env);
  const indexCommit = git(['commit-tree', indexTree, '-m', 'QA checkpoint index']);
  const workCommit = git(['commit-tree', workTree, '-p', indexCommit, '-m',
    `LUNA file checkpoint\nindex=${indexTree}\nat=${Date.now()}\noutcome=completed`]);
  const taskKey = crypto.createHash('sha256').update(chatId).digest('hex');
  git(['update-ref', `refs/luna/checkpoints/${taskKey}/${turnId}/${phase}`, workCommit]);
}
async function upload(id, bytes, filename = 'image.png') {
  await call('UploadChunk', { uploadId: id, seq: 0, data: Buffer.from(bytes).toString('base64') });
  const result = await call('UploadCommit', { uploadId: id, fileName: filename });
  const resolved = fs.realpathSync(result.path);
  assert.ok(resolved.startsWith(dataRoot + path.sep), 'Upload escaped the assigned QA data root');
  blobPaths.add(resolved);
  return resolved;
}
try {
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });
  const info = await call('EngineInfo');
  assert.equal(typeof info.deviceId, 'string');
  git(['init', '-q', '-b', 'main']);
  git(['config', 'core.hooksPath', path.join(fixture, 'no-hooks')]);
  fs.writeFileSync(path.join(repo, 'note.txt'), 'base\n');
  fs.writeFileSync(path.join(repo, '.gitignore'), 'ignored.txt\n');
  git(['add', '.']);
  git(['commit', '-q', '-m', 'QA base']);
  const head = git(['rev-parse', 'HEAD']);
  await mutate({ op: 'createSpace', spaceId, deviceId: info.deviceId, path: repo, name: 'Disposable workspace QA', gitDetected: true });
  projectCreated = true;
  await mutate({ op: 'createChat', chatId, spaceId, cwd: repo });
  chatCreated = true;
  await check('native file read/write and stale-save conflict', async () => {
    const read = await call('ReadWorkspaceFile', { chatId, path: 'note.txt' });
    assert.equal(read.text, 'base\n');
    const request = { chatId, path: 'note.txt', expectedCheckoutId: read.checkoutId,
      expectedContentHash: read.contentHash, encoding: 'utf8', lineEnding: 'lf', text: 'saved\n' };
    assert.equal((await call('WriteWorkspaceFile', request)).status, 'written');
    assert.equal((await call('WriteWorkspaceFile', { ...request, text: 'stale overwrite\n' })).status, 'conflict');
    assert.equal(fs.readFileSync(path.join(repo, 'note.txt'), 'utf8'), 'saved\n');
  });
  await check('native default-worker diff RPC and exact sources', async () => {
    const diff = await call('GetCheckoutDiff', { cwd: repo, mode: 'working' });
    const source = await call('GetCheckoutFileDiffText', { cwd: repo, checkoutId: diff.checkoutId,
      mode: 'working', path: 'note.txt', diffChecksum: diff.checksum });
    assert.equal(source.oldText, 'base\n');
    assert.equal(source.newText, 'saved\n');
    assert.equal(source.stale, false);
    assert.equal((await call('EngineInfo')).deviceId, info.deviceId);
  });
  await check('native immutable upload collision and retry', async () => {
    const original = await upload(uploadId + '-first', 'original');
    await call('UploadChunk', { uploadId: uploadId + '-second', seq: 0, data: Buffer.from('replacement').toString('base64') });
    await assert.rejects(call('UploadCommit', { uploadId: uploadId + '-second', fileName: 'image.png' }), /different or unsafe bytes/);
    const read = await call('ReadAttachmentChunk', { path: original, offset: 0 });
    assert.equal(Buffer.from(read.data, 'base64').toString(), 'original');
    const retry = await call('UploadCommit', { uploadId: uploadId + '-second', fileName: 'retry.png' });
    assert.ok(fs.realpathSync(retry.path).startsWith(dataRoot + path.sep));
    blobPaths.add(fs.realpathSync(retry.path));
    assert.equal(fs.readFileSync(retry.path, 'utf8'), 'replacement');
    assert.equal(await upload(uploadId + '-first', 'original'), original);
  });
  await check('native checkpoint fixture readback and shared-restore refusal', async () => {
    fs.writeFileSync(path.join(repo, 'ignored.txt'), 'staged\n');
    git(['add', '-f', 'ignored.txt']);
    const indexTree = git(['write-tree']);
    fs.writeFileSync(path.join(repo, 'ignored.txt'), 'before\n');
    seedCheckpoint('before');
    fs.writeFileSync(path.join(repo, 'ignored.txt'), 'after\n');
    seedCheckpoint('after');
    const list = await call('ListGitCheckpoints', { chatId, cwd: repo });
    const row = list.checkpoints.find(row => row.turnId === turnId);
    assert.ok(row?.beforeAvailable && row?.afterAvailable);
    assert.equal(list.restoreAvailable, false);
    const diff = await call('DiffGitCheckpoint', { chatId, cwd: repo, turnId });
    assert.deepEqual(diff.paths, ['ignored.txt']);
    assert.ok(diff.patch.includes('-before') && diff.patch.includes('+after'));
    await assert.rejects(call('PreviewGitCheckpointRestore', { chatId, cwd: repo, turnId, point: 'before' }), /isolated worktree/);
    assert.equal(git(['write-tree']), indexTree);
    assert.equal(git(['rev-parse', 'HEAD']), head);
    assert.equal(fs.readFileSync(path.join(repo, 'ignored.txt'), 'utf8'), 'after\n');
  });
} catch (error) {
  failure = error;
  evidence('failure', { name: error.name, message: error.message, stack: error.stack });
} finally {
  try {
    if (ws.readyState === WebSocket.OPEN && projectCreated) {
      await mutate({ op: 'deleteSpace', spaceId, expectedChatIds: chatCreated ? [chatId] : [] });
      const projectCleanup = await call('GetProjectCleanup', { projectId: spaceId });
      assert.equal(projectCleanup?.state, 'completed');
      const taskCleanup = chatCreated ? await call('GetTaskCleanup', { taskId: chatId }) : null;
      if (chatCreated) assert.equal(taskCleanup?.state, 'completed');
      evidence('cleanup', { projectDeleted: spaceId, chatDeleted: chatCreated ? chatId : null, projectCleanup, taskCleanup });
    }
    for (const file of blobPaths) {
      assert.equal(fs.realpathSync(dataDir), dataRoot, 'QA data root changed before cleanup');
      assert.ok(file.startsWith(dataRoot + path.sep));
      if (fs.existsSync(file)) assert.equal(fs.realpathSync(file), file, 'Upload cleanup path changed');
      fs.rmSync(file, { force: true });
    }
    // Only exact returned files are removed; failed staging remains QA-owned
    // and is reclaimed by the engine's normal expiry policy.
    fs.rmSync(fixture, { recursive: true, force: true });
  } catch (error) {
    evidence('cleanupFailure', { message: error.message });
    failure ||= error;
  }
  ws.close();
  fs.writeFileSync(path.join(report, 'result.json'), JSON.stringify({ passed: checks, ok: !failure,
    checkoutRevision: execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
    builtRevision: process.env.LUNA_NATIVE_BUILD_REVISION || null,
    port, appPid, appExecutable, appName, bundleId, dataRoot,
    signatureVerified: true, mcpIsolation: plist.LSEnvironment.LUNA_QA_MCP_ISOLATION,
    fixture, fixtureRemoved: !fs.existsSync(fixture),
    chatId, spaceId, error: failure?.message ?? null,
    checkpointScope: 'Fixture-seeded Git refs; native read/diff and guarded refusal, no provider capture or isolated restore' }, null, 2) + '\n');
}
if (failure) throw failure;
console.log(`Artifacts: ${report}`);
