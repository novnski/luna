#!/usr/bin/env node
// Real-app acceptance: removing a project keeps its sessions, and adding the
// same folder again brings every one of them back.
//
// Failure modes (written before the phases):
//   1. Removing the project deletes its sessions or their task state.
//   2. A removed project's sessions are still listed (sidebar, archive, board).
//   3. The removed project still appears as a live project.
//   4. Re-adding the folder mints a second project instead of restoring.
//   5. Restored sessions lose their project, title, settled or archived state.
//   6. Removing one project hides or changes another project's sessions.
//
// Phases (the operator drives the real UI between them):
//   seed             fixture project with open, settled and archived sessions,
//                    plus a control project
//   verify-removed   project row kept with removedAt; its sessions hidden from
//                    WatchChats but still resolvable; control untouched
//   verify-restored  exactly one project at the folder, original id, every
//                    session back with its seeded state
//   capture <label>  window screenshot of the QA app into the run evidence
//
// Requires all four LUNA_DEV_* QA identity values (never the primary app).
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';

const root = fs.realpathSync(path.resolve(import.meta.dirname, '..'));
const keys = ['LUNA_DEV_APP_NAME', 'LUNA_DEV_BUNDLE_ID', 'LUNA_DEV_IPC_PORT', 'LUNA_DEV_DATA_DIR'];
for (const key of keys) assert.ok(process.env[key], `${key} is required (QA identity only)`);
const appName = process.env.LUNA_DEV_APP_NAME;
const port = Number(process.env.LUNA_DEV_IPC_PORT);
assert.match(appName, /^LUNA QA [A-Za-z0-9][A-Za-z0-9 -]{0,48}$/);
assert.ok(port >= 1024 && port <= 65535 && port !== 59777, 'a separate QA IPC port is required');
const dataDir = fs.realpathSync(process.env.LUNA_DEV_DATA_DIR);
const target = path.join(root, 'target');
assert.ok(dataDir.startsWith(`${target}${path.sep}`), 'QA data must be inside this checkout target');
const bundle = path.join(target, 'luna-macos-dev', `${appName}.app`);
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], { encoding: 'utf8' }));
assert.equal(plist.CFBundleIdentifier, process.env.LUNA_DEV_BUNDLE_ID);
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, dataDir);
const pids = execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], { encoding: 'utf8' }).match(/^p\d+$/gm) ?? [];
assert.equal(new Set(pids).size, 1, 'exactly one QA app must be listening');
const appPid = Number(pids[0].slice(1));
assert.equal(execFileSync('ps', ['-p', String(appPid), '-o', 'comm='], { encoding: 'utf8' }).trim(),
  fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna')), 'listener must be the QA bundle');

const [phase, ...rest] = process.argv.slice(2);
const evidenceRoot = path.join(target, 'qa', 'project-remove-restore');
const latest = path.join(evidenceRoot, 'latest');
const runDir = phase === 'seed'
  ? path.join(evidenceRoot, `run-${new Date().toISOString().replace(/[:.]/g, '-')}`)
  : fs.realpathSync(latest);
fs.mkdirSync(runDir, { recursive: true });
const statePath = path.join(runDir, 'state.json');
const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => fs.writeFileSync(path.join(runDir, name), JSON.stringify(value, null, 2) + '\n');
const log = (name, value) => fs.appendFileSync(path.join(runDir, 'events.jsonl'),
  JSON.stringify({ at: new Date().toISOString(), phase, name, value }) + '\n');

if (phase === 'capture') {
  const label = (rest[0] ?? 'capture').replace(/[^a-z0-9._-]+/gi, '-');
  const jxa = script => execFileSync('osascript', ['-l', 'JavaScript', '-e', script], { encoding: 'utf8' }).trim();
  const windows = JSON.parse(jxa(`ObjC.import("CoreGraphics");
JSON.stringify(ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, $.kCGNullWindowID)))
  .filter(w => w.kCGWindowOwnerPID === ${appPid} && w.kCGWindowLayer === 0)
  .map(w => ({number: w.kCGWindowNumber, area: w.kCGWindowBounds.Width * w.kCGWindowBounds.Height})));`));
  windows.sort((a, b) => b.area - a.area);
  assert.ok(windows[0], 'the QA app has no on-screen window');
  fs.mkdirSync(path.join(runDir, 'screenshots'), { recursive: true });
  const file = path.join(runDir, 'screenshots', `${label}.png`);
  const shot = spawnSync('screencapture', ['-x', '-o', `-l${windows[0].number}`, file], { encoding: 'utf8' });
  assert.equal(shot.status, 0, `screencapture failed: ${shot.stderr}`);
  log('capture', { label, file: path.relative(runDir, file), sha256: sha256(fs.readFileSync(file)) });
  console.log(file);
  process.exit(0);
}

const ws = new WebSocket(`ws://127.0.0.1:${port}`);
let nextId = 1;
function call(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', listener); reject(new Error(`${method} timeout`)); }, 15000);
    function listener(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timer);
      ws.removeEventListener('message', listener);
      log(method, frame);
      if (frame.err) reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      else resolve(frame.ok);
    }
    ws.addEventListener('message', listener);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
// First item of a watch stream: the current catalog.
function firstItem(method) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', listener); reject(new Error(`${method} timeout`)); }, 15000);
    function listener(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      if (frame.err) { clearTimeout(timer); ws.removeEventListener('message', listener); reject(new Error(`${method}: ${JSON.stringify(frame.err)}`)); }
      if (frame.item === undefined) return;
      clearTimeout(timer);
      ws.removeEventListener('message', listener);
      resolve(frame.item);
    }
    ws.addEventListener('message', listener);
    ws.send(JSON.stringify({ id, method, params: {} }));
  });
}
const snapshot = taskId => call('GetTaskCommandSnapshot', { taskId });
async function command(taskId, intent, expect = true) {
  const commandId = crypto.randomUUID();
  const envelope = { commandId, actorId: 'qa', clientId: 'qa-project-remove-restore', clientSessionId: 'qa', taskId, intent };
  if (expect) {
    const read = await snapshot(taskId);
    envelope.expectedTaskRevision = read.snapshot.taskRevision;
    if (read.snapshot.bindingGeneration != null) envelope.expectedBindingGeneration = read.snapshot.bindingGeneration;
  }
  let receipt = await call('ExecuteTaskCommand', envelope);
  const deadline = Date.now() + 15000;
  while (receipt?.status === 'pending' && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 50));
    receipt = await call('GetTaskCommandReceipt', { commandId });
  }
  assert.equal(receipt?.status, 'applied', JSON.stringify(receipt));
}
const pick = row => ({ id: row.id, spaceId: row.spaceId ?? null, title: row.title ?? null,
  archived: !!row.archived, settlementOverride: row.settlementOverride ?? null });
const checks = [];
function check(name, ok, detail) {
  assert.ok(ok, `${name}: ${JSON.stringify(detail)}`);
  checks.push(name);
  console.log(`PASS ${name}`);
}

try {
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });
  const { deviceId } = await call('EngineInfo');
  if (phase === 'seed') {
    const fixture = path.join(runDir, 'fixture');
    const folder = path.join(fixture, 'remember-me');
    const controlFolder = path.join(fixture, 'control-project');
    fs.mkdirSync(folder, { recursive: true });
    fs.mkdirSync(controlFolder, { recursive: true });
    fs.writeFileSync(path.join(folder, 'README.md'), 'Fixture project for remove/restore QA.\n');
    const projectId = crypto.randomUUID();
    const controlId = crypto.randomUUID();
    await call('Mutate', { op: 'createSpace', spaceId: projectId, deviceId, path: fs.realpathSync(folder) });
    await call('Mutate', { op: 'createSpace', spaceId: controlId, deviceId, path: fs.realpathSync(controlFolder) });
    const sessions = [
      { key: 'open', title: 'Open thread — keep me', projectId },
      { key: 'settled', title: 'Settled thread — keep me', projectId, settled: true },
      { key: 'archived', title: 'Archived thread — keep me', projectId, archived: true },
      { key: 'control', title: 'Control project thread', projectId: controlId },
    ].map(session => ({ ...session, id: crypto.randomUUID() }));
    for (const session of sessions) {
      await command(session.id, { kind: 'createChat', spaceId: session.projectId }, false);
      await command(session.id, { kind: 'rename', title: session.title });
      if (session.settled) await command(session.id, { kind: 'setSettled', settled: true });
      if (session.archived) await command(session.id, { kind: 'setArchived', archived: true });
    }
    await new Promise(resolve => setTimeout(resolve, 300));
    const chats = await firstItem('WatchChats');
    const seeded = Object.fromEntries(sessions.map(session => [session.key, pick(chats.find(row => row.id === session.id) ?? {})]));
    const revisions = {};
    for (const session of sessions) revisions[session.key] = (await snapshot(session.id)).snapshot.taskRevision;
    for (const session of sessions) check(`seeded ${session.key}`, seeded[session.key].id === session.id, seeded[session.key]);
    check('settled seed is settled', seeded.settled.settlementOverride != null, seeded.settled);
    check('archived seed is archived', seeded.archived.archived, seeded.archived);
    fs.rmSync(latest, { force: true });
    fs.symlinkSync(path.basename(runDir), latest);
    save('state.json', { deviceId, projectId, controlId, folder: fs.realpathSync(folder), sessions, seeded, revisions });
  } else {
    const state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
    const spaces = await firstItem('WatchSpaces');
    const chats = await firstItem('WatchChats');
    const atFolder = spaces.filter(space => space.deviceId === state.deviceId && space.path === state.folder);
    const visible = id => chats.find(row => row.id === id);
    const projectSessions = state.sessions.filter(session => session.projectId === state.projectId);
    const control = state.sessions.find(session => session.key === 'control');
    check('control project is live', spaces.some(space => space.id === state.controlId && !space.removedAt), state.controlId);
    check('control session unchanged', JSON.stringify(pick(visible(control.id) ?? {})) === JSON.stringify(state.seeded.control),
      pick(visible(control.id) ?? {}));
    if (phase === 'verify-removed') {
      check('one row at the folder', atFolder.length === 1, atFolder);
      check('row is the original project', atFolder[0].id === state.projectId, atFolder[0]);
      check('row is marked removed', typeof atFolder[0].removedAt === 'string', atFolder[0]);
      for (const session of projectSessions) {
        check(`${session.key} hidden`, !visible(session.id), session.id);
        const read = await snapshot(session.id);
        check(`${session.key} still resolvable, unchanged`, read.snapshot.taskRevision === state.revisions[session.key],
          { now: read.snapshot.taskRevision, seeded: state.revisions[session.key] });
      }
    } else if (phase === 'verify-restored') {
      check('exactly one project at the folder', atFolder.length === 1, atFolder);
      check('restored the original project id', atFolder[0].id === state.projectId, atFolder[0]);
      check('project is live again', !atFolder[0].removedAt, atFolder[0]);
      for (const session of projectSessions) {
        check(`${session.key} restored with seeded state`,
          JSON.stringify(pick(visible(session.id) ?? {})) === JSON.stringify(state.seeded[session.key]),
          { now: pick(visible(session.id) ?? {}), seeded: state.seeded[session.key] });
      }
    } else {
      throw new Error(`unknown phase ${phase}`);
    }
    save(`${phase}.json`, { spaces: atFolder, chats: projectSessions.map(session => pick(visible(session.id) ?? { id: session.id })) });
  }
  save(`${phase}-checks.json`, { phase, passed: checks, at: new Date().toISOString() });
  console.log(`${phase}: ${checks.length} checks passed; evidence ${runDir}`);
} finally {
  ws.close();
}
