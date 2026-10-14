#!/usr/bin/env node
// CORE-11 / E11 native QA helper. It reads back LUNA's first-party OpenCode
// provider package through LUNA Dev's owner IPC (the same ListExtensions /
// ListExtensionProviders / GetProviderCatalog / GetProviderModels /
// ListExtensionInstances / SetExtensionEnabled paths as Settings), records
// which processes own each `opencode serve` (OS readback), hashes the app
// bundle before and after, and writes a SHA-256 manifest of the evidence.
// It never sends a prompt: messages, steer, Stop and the picker are driven
// in the app (docs/pending-tasks/extensible-core/core-11/README.md).
//
//   node scripts/qa-core11-opencode.mjs hash-app <App.app> <out.json>
//   node scripts/qa-core11-opencode.mjs readback <evidence dir> [--port N]
//   node scripts/qa-core11-opencode.mjs processes <evidence dir>
//   node scripts/qa-core11-opencode.mjs disable|enable <evidence dir> [--port N]
//   node scripts/qa-core11-opencode.mjs manifest <evidence dir>

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstatSync, mkdirSync, readFileSync, readdirSync, readlinkSync, writeFileSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const PACKAGE_ID = 'luna.provider/opencode';
const PROVIDER_ID = 'opencode';
const [command, ...rest] = process.argv.slice(2);
const portAt = rest.indexOf('--port');
const port = portAt >= 0 ? Number(rest[portAt + 1]) : 59777;
const positional =
  portAt >= 0 ? rest.filter((_, index) => index !== portAt && index !== portAt + 1) : rest;

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const write = (dir, name, value) => {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, name), `${JSON.stringify(value, null, 2)}\n`);
  console.log(`wrote ${join(dir, name)}`);
};

function walk(root) {
  const files = {};
  const pending = [root];
  while (pending.length > 0) {
    const dir = pending.pop();
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name);
      const key = relative(root, path);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) files[key] = `link:${readlinkSync(path)}`;
      else if (stat.isDirectory()) pending.push(path);
      else files[key] = sha256(readFileSync(path));
    }
  }
  return Object.fromEntries(Object.entries(files).sort(([a], [b]) => a.localeCompare(b)));
}

async function owner() {
  const socket = new WebSocket(`ws://127.0.0.1:${port}`);
  await new Promise((resolveOpen, reject) => {
    socket.addEventListener('open', () => resolveOpen(), { once: true });
    socket.addEventListener('error', () => reject(new Error(`LUNA Dev's owner IPC is not reachable on port ${port}`)), { once: true });
  });
  let id = 0;
  const pending = new Map();
  socket.addEventListener('message', ({ data }) => {
    for (const line of String(data).split('\n').filter(Boolean)) {
      const frame = JSON.parse(line);
      pending.get(frame.id)?.(frame);
      pending.delete(frame.id);
    }
  });
  const call = (method, params = {}) =>
    new Promise((resolveCall, reject) => {
      id += 1;
      const timer = setTimeout(() => reject(new Error(`${method} timed out`)), 120_000);
      pending.set(id, (frame) => {
        clearTimeout(timer);
        if ('err' in frame) reject(new Error(`${method}: ${frame.err}`));
        else resolveCall(frame.ok);
      });
      socket.send(JSON.stringify({ id, method, params }));
    });
  return { call, close: () => socket.close() };
}

async function installedPackage(call) {
  const listed = await call('ListExtensions');
  const found = listed.packages.find((item) => item.id === PACKAGE_ID);
  if (!found) throw new Error(`${PACKAGE_ID} is not installed`);
  return found;
}

/// Every `opencode serve` and package worker, with its parent: a turn's
/// server is a child of a `luna-opencode-worker`, never of `luna` itself.
function processes() {
  const listing = execFileSync('ps', ['-axo', 'pid=,ppid=,command='], { encoding: 'utf8' });
  const rows = listing
    .split('\n')
    .map((line) => line.trim().match(/^(\d+)\s+(\d+)\s+(.*)$/))
    .filter(Boolean)
    .map(([, pid, ppid, cmd]) => ({ pid: Number(pid), ppid: Number(ppid), command: cmd }));
  const workers = rows.filter((row) => /luna-opencode-worker/.test(row.command));
  const servers = rows
    .filter((row) => /opencode(\.exe)?\s+serve\b/.test(row.command))
    .map((row) => ({ ...row, ownedByWorker: workers.some((worker) => worker.pid === row.ppid) }));
  const app = rows.filter((row) => /\/Contents\/MacOS\/luna(\s|$)/.test(row.command));
  return { at: new Date().toISOString(), app, workers, servers };
}

async function readback(call, dir) {
  const providers = await call('ListExtensionProviders');
  write(dir, 'providers.json', providers);
  let pkg = null;
  try {
    pkg = await installedPackage(call);
  } catch (error) {
    pkg = { error: String(error.message ?? error) };
  }
  write(dir, 'package.json', pkg);
  const catalog = await call('GetProviderCatalog');
  write(dir, 'catalog.json', {
    hostId: catalog.hostId,
    registryId: catalog.registryId,
    generation: catalog.generation,
    rows: catalog.providers.filter((row) => row.id === PROVIDER_ID),
    providerIds: catalog.providers.map((row) => row.id),
  });
  try {
    write(dir, 'models.json', await call('GetProviderModels', { harness: PROVIDER_ID, refresh: true }));
  } catch (error) {
    write(dir, 'models.json', { error: String(error.message ?? error) });
  }
  const instances = await call('ListExtensionInstances');
  write(dir, 'instances.json', {
    ...instances,
    instances: instances.instances
      .filter((view) => view.extensionId === PACKAGE_ID)
      .map(({ logs, ...view }) => ({ ...view, logLines: logs?.length ?? 0 })),
  });
  write(dir, 'processes.json', processes());
}

const usage = () => {
  console.error('usage: qa-core11-opencode.mjs hash-app|readback|processes|disable|enable|manifest ...');
  process.exit(2);
};

switch (command) {
  case 'hash-app': {
    const [bundle, out] = positional;
    if (!bundle || !out) usage();
    const files = walk(resolve(bundle));
    let signature;
    try {
      execFileSync('codesign', ['--verify', '--deep', '--strict', resolve(bundle)], { stdio: 'pipe' });
      signature = { verified: true };
    } catch (error) {
      signature = { verified: false, detail: String(error.stderr ?? error.message) };
    }
    let details = '';
    try {
      details = execFileSync('codesign', ['-dv', '--verbose=2', resolve(bundle)], { stdio: ['ignore', 'pipe', 'pipe'] }).toString();
    } catch (error) {
      details = String(error.stderr ?? '');
    }
    const worker = join(resolve(bundle), 'Contents', 'MacOS', 'luna-opencode-worker');
    let workerSha256 = null;
    try {
      workerSha256 = sha256(readFileSync(worker));
    } catch {
      workerSha256 = null;
    }
    const value = {
      bundle: resolve(bundle),
      fileCount: Object.keys(files).length,
      digest: sha256(JSON.stringify(files)),
      workerSha256,
      signature,
      codesign: details.split('\n').filter((line) => /^(CDHash|Identifier|TeamIdentifier|Timestamp)/.test(line)),
      files,
    };
    mkdirSync(resolve(out, '..'), { recursive: true });
    writeFileSync(resolve(out), `${JSON.stringify(value, null, 2)}\n`);
    console.log(JSON.stringify({ digest: value.digest, fileCount: value.fileCount, workerSha256, signature }, null, 2));
    break;
  }
  case 'readback': {
    const [dir] = positional;
    if (!dir) usage();
    const { call, close } = await owner();
    try {
      await readback(call, dir);
    } finally {
      close();
    }
    break;
  }
  case 'processes': {
    const [dir] = positional;
    if (!dir) usage();
    write(dir, `processes-${Date.now()}.json`, processes());
    break;
  }
  case 'disable':
  case 'enable': {
    const [dir] = positional;
    if (!dir) usage();
    const { call, close } = await owner();
    try {
      const found = await installedPackage(call);
      const grants = found.capabilities.map((capability) => capability.id);
      const changed = await call('SetExtensionEnabled', {
        extensionId: found.id,
        packageHash: found.packageHash,
        enabled: command === 'enable',
        grants: command === 'enable' ? grants : [],
      });
      write(join(dir, `after-${command}`), 'set-enabled.json', changed);
      await readback(call, join(dir, `after-${command}`));
    } finally {
      close();
    }
    break;
  }
  case 'manifest': {
    const [dir] = positional;
    if (!dir) usage();
    const root = resolve(dir);
    const lines = Object.entries(walk(root))
      .filter(([path]) => path !== 'SHA256SUMS')
      .map(([path, digest]) => `${digest}  ${path}`);
    writeFileSync(join(root, 'SHA256SUMS'), `${lines.join('\n')}\n`);
    console.log(`wrote ${join(root, 'SHA256SUMS')} (${lines.length} files)`);
    break;
  }
  default:
    usage();
}
