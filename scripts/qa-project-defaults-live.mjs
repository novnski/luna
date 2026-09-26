#!/usr/bin/env node
// Repeatable native-app setup/readback. Only the isolated Defaults QA port is
// accepted; this script never sends prompts or invokes providers.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
const port = Number(process.env.LUNA_IPC_PORT || 59788);
assert.equal(port, 59788, 'Only LUNA QA Defaults port 59788 is allowed');
const mode = process.argv[2] || 'read';
assert.ok(['seed', 'read', 'clear'].includes(mode));
const root = path.resolve(import.meta.dirname, '..');
const output = path.join(root, 'target/qa/project-defaults');
fs.mkdirSync(output, { recursive: true });
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
