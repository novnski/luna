#!/usr/bin/env node
// CORE-08 / E10 native QA helper. It installs, enables, reads back and
// disables an external provider package through LUNA Dev's owner IPC (the
// same InstallExtension / SetExtensionEnabled path as Settings → Extensions),
// hashes the app bundle before and after, and writes a SHA-256 manifest of
// the evidence. It never sends a prompt: messages, Stop and the picker are
// driven in the app (docs/pending-tasks/extensible-core/core-08/README.md).
//
//   node scripts/qa-core08-provider.mjs hash-app <App.app> <out.json>
//   node scripts/qa-core08-provider.mjs install <package dir> <evidence dir> [--port N]
//   node scripts/qa-core08-provider.mjs readback <evidence dir> [--port N]
//   node scripts/qa-core08-provider.mjs disable|enable <evidence dir> [--port N]
//   node scripts/qa-core08-provider.mjs manifest <evidence dir>

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstatSync, mkdirSync, readFileSync, readdirSync, readlinkSync, writeFileSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const PROVIDER_ID = 'novnski.gemini/gemini-cli';
const NATIVE_CODE = 'luna.capability/native-code';
const [command, ...rest] = process.argv.slice(2);
const portAt = rest.indexOf('--port');
const port = portAt >= 0 ? Number(rest[portAt + 1]) : 59777;
const positional = rest.filter((_, index) => index !== portAt && index !== portAt + 1);

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
      const timer = setTimeout(() => reject(new Error(`${method} timed out`)), 60_000);
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
  const found = listed.packages.find((item) => item.id === PROVIDER_ID);
  if (!found) throw new Error(`${PROVIDER_ID} is not installed`);
  return found;
}

async function readback(call, dir) {
  const providers = await call('ListExtensionProviders');
  write(dir, 'providers.json', providers);
  const catalog = await call('GetProviderCatalog');
  write(dir, 'catalog.json', {
    hostId: catalog.hostId,
    registryId: catalog.registryId,
    generation: catalog.generation,
    provider: catalog.providers.find((row) => row.id === PROVIDER_ID) ?? null,
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
    instances: instances.instances.filter((view) => view.extensionId === PROVIDER_ID),
  });
}

const usage = () => {
  console.error('usage: qa-core08-provider.mjs hash-app|install|readback|disable|enable|manifest ...');
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
    const value = {
      bundle: resolve(bundle),
      fileCount: Object.keys(files).length,
      digest: sha256(JSON.stringify(files)),
      signature,
      codesign: details.split('\n').filter((line) => /^(CDHash|Identifier|TeamIdentifier|Timestamp)/.test(line)),
      files,
    };
    mkdirSync(resolve(out, '..'), { recursive: true });
    writeFileSync(resolve(out), `${JSON.stringify(value, null, 2)}\n`);
    console.log(JSON.stringify({ digest: value.digest, fileCount: value.fileCount, signature }, null, 2));
    break;
  }
  case 'install': {
    const [packageDir, dir] = positional;
    if (!packageDir || !dir) usage();
    const manifestBytes = readFileSync(join(resolve(packageDir), 'luna-plugin.json'));
    const manifest = JSON.parse(manifestBytes);
    const packageHash = sha256(manifestBytes);
    const grants = [NATIVE_CODE, ...manifest.capabilities.map((capability) => capability.id)];
    const { call, close } = await owner();
    try {
      const installed = await call('InstallExtension', { path: resolve(packageDir), expectedPackageHash: packageHash });
      const enabled = await call('SetExtensionEnabled', {
        extensionId: manifest.id,
        packageHash,
        enabled: true,
        grants: [...new Set(grants)],
      });
      write(dir, 'install.json', { packageHash, installed, enabled });
      await readback(call, dir);
    } finally {
      close();
    }
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
      write(join(dir, `after-${command}`), 'package.json', changed);
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
