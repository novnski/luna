#!/usr/bin/env node
// Repeatable native-app setup/readback. Only the isolated Defaults QA port is
// accepted; this script never sends prompts or invokes providers.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import assert from 'node:assert/strict';
const port = Number(process.env.LUNA_IPC_PORT || 59788);
assert.equal(port, 59788, 'Only LUNA QA Defaults port 59788 is allowed');
const mode = process.argv[2] || 'read';
assert.ok(['seed', 'read', 'clear', 'seed-command-tasks'].includes(mode));
const root = path.resolve(import.meta.dirname, '..');
const output = path.join(root, 'target/qa/project-defaults');
fs.mkdirSync(output, { recursive: true });

function command(cwd, args) {
  return execFileSync('git', ['-C', cwd, ...args], { encoding: 'utf8' }).trim();
}
function qaAppIdentity() {
  assert.equal(process.platform, 'darwin');
  const bundle = path.join(root, 'target/luna-macos-dev/LUNA QA Defaults.app');
  const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
  const plist = JSON.parse(execFileSync('plutil', [
    '-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist'),
  ], { encoding: 'utf8' }));
  assert.equal(plist.CFBundleIdentifier, 'com.novnski.luna.qa.defaults');
  assert.equal(plist.CFBundleDisplayName, 'LUNA QA Defaults');
  assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, '59788');
  assert.equal(path.resolve(plist.LSEnvironment.LUNA_DATA_DIR),
    path.join(root, 'target/luna-qa-data/defaults'));
  execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
  const listeners = [...new Set(execFileSync('lsof', [
    '-nP', '-iTCP:59788', '-sTCP:LISTEN', '-Fp',
  ], { encoding: 'utf8' }).match(/^p[0-9]+$/gm)?.map(line => Number(line.slice(1))) ?? [])];
  assert.equal(listeners.length, 1, 'one exact QA app listener is required');
  const pid = listeners[0];
  const running = execFileSync('ps', ['-ww', '-p', String(pid), '-o', 'command='],
    { encoding: 'utf8' }).trim();
  assert.ok(running === executable || running.startsWith(`${executable} `),
    'IPC listener does not belong to this signed QA bundle');
  return { bundle, executable, pid, dataDir: plist.LSEnvironment.LUNA_DATA_DIR };
}
function linkedCheckout(cwd, repository, qaRoot) {
  assert.ok(path.isAbsolute(cwd), 'linked checkout path must be absolute');
  const checkout = fs.realpathSync(cwd);
  assert.ok(checkout.startsWith(`${qaRoot}${path.sep}`), 'checkout escapes QA fixtures');
  assert.notEqual(checkout, repository, 'use a linked checkout, not the base repo');
  assert.ok(fs.statSync(path.join(checkout, '.git')).isFile(),
    'linked checkout must have a .git file');
  assert.equal(fs.realpathSync(command(checkout, ['rev-parse', '--show-toplevel'])), checkout);
  const common = fs.realpathSync(path.resolve(checkout,
    command(checkout, ['rev-parse', '--git-common-dir'])));
  const expectedCommon = fs.realpathSync(path.resolve(repository,
    command(repository, ['rev-parse', '--git-common-dir'])));
  assert.equal(common, expectedCommon, 'checkout belongs to a different repository');
  const branch = command(checkout, ['branch', '--show-current']);
  assert.ok(branch && branch !== 'HEAD', 'QA checkout needs an attached branch');
  const stat = fs.statSync(checkout, { bigint: true });
  return { cwd: checkout, branch, inode: String(stat.ino), createdNs: String(stat.birthtimeNs) };
}
function savePlan(file, value) {
  const staged = `${file}.${process.pid}.${randomUUID()}.tmp`;
  const fd = fs.openSync(staged, 'wx');
  try {
    fs.writeFileSync(fd, `${JSON.stringify(value, null, 2)}\n`);
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
  fs.renameSync(staged, file);
}
const verifiedApp = mode === 'seed-command-tasks' ? qaAppIdentity() : null;
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { ws.removeEventListener('message', onMessage); reject(new Error(`${method} timed out`)); }, 45000);
    function onMessage(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timeout);
      ws.removeEventListener('message', onMessage);
      if (frame.err) reject(new Error(frame.err));
      else {
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
        resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    }
    ws.addEventListener('message', onMessage);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
try {
  const info = await call('EngineInfo');
  assert.ok(info.deviceId);
  if (mode === 'seed-command-tasks') {
    assert.equal(info.workspaceScope, 'local', 'Commands fixture must remain local');
    assert.equal(process.argv.length, 5, 'pass absolute A1 and A2 checkout paths');
    const spaces = await call('WatchSpaces');
    const project = spaces.find(space => space.id === 'qa-defaults-a');
    const repository = fs.realpathSync(path.join(output, 'project-a'));
    assert.ok(project && project.gitDetected && project.deviceId === info.deviceId);
    assert.equal(fs.realpathSync(project.path), repository);
    const qaRoot = fs.realpathSync(path.join(root, 'target/qa'));
    const checkouts = process.argv.slice(3).map(cwd => linkedCheckout(cwd, repository, qaRoot));
    assert.notEqual(checkouts[0].cwd, checkouts[1].cwd);
    const file = path.join(output, 'command-tasks.json');
    let plan;
    if (fs.existsSync(file)) {
      plan = JSON.parse(fs.readFileSync(file, 'utf8'));
      assert.equal(plan.deviceId, info.deviceId);
      assert.equal(plan.projectId, project.id);
      assert.deepEqual(plan.tasks.map(task => task.checkout), checkouts,
        'fixture checkout changed; inspect prior tasks before seeding');
    } else {
      const existing = await call('WatchChats');
      assert.ok(!existing.some(chat => chat.spaceId === project.id
        && checkouts.some(checkout => checkout.cwd === chat.cwd)),
      'a task already exists in a chosen QA checkout; inspect it first');
      plan = {
        version: 1, deviceId: info.deviceId, projectId: project.id,
        app: verifiedApp, clientSessionId: randomUUID(),
        tasks: checkouts.map((checkout, index) => ({
          role: index ? 'A2' : 'A1', taskId: randomUUID(),
          commandId: randomUUID(), checkout, receipt: null, status: 'planned',
        })),
      };
      savePlan(file, plan); // Preserve IDs before the first RPC crosses the wire.
    }
    for (const task of plan.tasks) {
      const chats = await call('WatchChats');
      const existing = chats.find(chat => chat.id === task.taskId);
      if (existing) {
        assert.equal(existing.spaceId, project.id);
        assert.equal(existing.deviceId, info.deviceId);
        assert.equal(fs.realpathSync(existing.cwd), task.checkout.cwd);
        assert.equal(existing.branch, task.checkout.branch,
          'seeded task branch differs from its linked checkout');
        task.readback = { chatId: existing.id, spaceId: existing.spaceId,
          deviceId: existing.deviceId, cwd: existing.cwd, branch: existing.branch };
        task.receipt = await call('GetTaskCommandReceipt', { commandId: task.commandId });
        assert.equal(task.receipt?.status, 'applied');
        task.status = 'applied';
        savePlan(file, plan);
        continue;
      }
      assert.equal(task.status, 'planned',
        'a prior command may be pending; inspect its receipt before retrying');
      task.status = 'issuing';
      savePlan(file, plan);
      const receipt = await call('ExecuteTaskCommand', {
        commandId: task.commandId, taskId: task.taskId,
        actorId: info.deviceId, clientId: 'luna-qa-defaults',
        clientSessionId: plan.clientSessionId,
        intent: { kind: 'createChat', spaceId: project.id,
          cwd: task.checkout.cwd, branch: task.checkout.branch },
      });
      task.receipt = receipt;
      task.status = receipt.status;
      savePlan(file, plan);
      assert.ok(['pending', 'applied'].includes(receipt.status),
        `CreateChat ended as ${receipt.status}`);
      let visible = false;
      for (let attempt = 0; attempt < 20; attempt++) {
        const rows = await call('WatchChats');
        const row = rows.find(chat => chat.id === task.taskId);
        if (row) {
          assert.equal(row.spaceId, project.id);
          assert.equal(row.deviceId, info.deviceId);
          assert.equal(fs.realpathSync(row.cwd), task.checkout.cwd);
          assert.equal(row.branch, task.checkout.branch,
            'created task branch differs from its linked checkout');
          task.readback = { chatId: row.id, spaceId: row.spaceId,
            deviceId: row.deviceId, cwd: row.cwd, branch: row.branch };
          task.receipt = await call('GetTaskCommandReceipt', { commandId: task.commandId });
          assert.equal(task.receipt?.status, 'applied');
          task.status = 'applied';
          savePlan(file, plan);
          visible = true;
          break;
        }
        await new Promise(resolve => setTimeout(resolve, 250));
      }
      assert.ok(visible, 'accepted CreateChat is not visible; inspect the saved command ID');
    }
    fs.writeFileSync(path.join(output, 'command-tasks-readback.json'),
      `${JSON.stringify(plan, null, 2)}\n`);
    console.log(JSON.stringify({ artifact: file, appPid: verifiedApp.pid,
      tasks: plan.tasks.map(task => ({ role: task.role, taskId: task.taskId,
        cwd: task.checkout.cwd, status: task.status })) }));
  }
  if (mode === 'seed') {
    for (const name of ['a', 'b']) {
      const repo = path.join(output, `project-${name}`);
      fs.mkdirSync(repo, { recursive: true });
      if (!fs.existsSync(path.join(repo, '.git'))) {
        execFileSync('git', ['init', '-b', 'main', repo], { stdio: 'ignore' });
        fs.writeFileSync(path.join(repo, 'README.md'), `Luna defaults QA ${name}\n`);
        execFileSync('git', ['-C', repo, 'add', 'README.md']);
        execFileSync('git', ['-C', repo, '-c', 'user.name=Luna QA', '-c', 'user.email=qa@example.invalid', 'commit', '-m', 'QA fixture'], { stdio: 'ignore' });
      }
      await call('Mutate', { op: 'createSpace', spaceId: `qa-defaults-${name}`, deviceId: info.deviceId, path: repo, name: `Defaults ${name.toUpperCase()}`, gitDetected: true });
    }
  }
  if (mode === 'clear') {
    const spaces = await call('WatchSpaces');
    for (const space of spaces.filter(space => ['qa-defaults-a', 'qa-defaults-b'].includes(space.id))) {
      await call('Mutate', { op: 'setProjectExecutionDefaults', spaceId: space.id, defaults: {}, expectedDefaults: space.executionDefaults || {} });
    }
  }
  const spaces = (await call('WatchSpaces')).filter(space => ['qa-defaults-a', 'qa-defaults-b'].includes(space.id));
  const chats = (await call('WatchChats')).filter(chat => spaces.some(space => space.id === chat.spaceId)).map(({ id, spaceId, config, cwd }) => ({ id, spaceId, config, cwd }));
  const evidence = { time: new Date().toISOString(), mode, port, deviceId: info.deviceId, spaces, chats };
  fs.writeFileSync(path.join(output, `live-${mode}.json`), JSON.stringify(evidence, null, 2));
  console.log(JSON.stringify({ artifact: path.join(output, `live-${mode}.json`), projects: spaces.map(({ name, executionDefaults }) => ({ name, executionDefaults })), tasks: chats.length }));
} finally { ws.close(); }
