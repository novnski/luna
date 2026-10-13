#!/usr/bin/env node
// Pack this provider into a LUNA package directory: copy its files, vendor
// the published @luna/sdk provider module, and write luna-plugin.json with
// every file's size and SHA-256 (CORE-06 manifest version 1). Nothing here
// talks to LUNA; install the result with InstallExtension (or Settings →
// Extensions → Install…).
//
//   node scripts/pack.mjs --out <dir> [--sdk <path to @luna/sdk src/provider.ts>]
//
// Prints {"dir", "packageHash"}. The package hash is the SHA-256 of the exact
// luna-plugin.json bytes, which pin every other file.

import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ID = 'novnski.gemini/gemini-cli';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const option = (name) => {
  const at = args.indexOf(name);
  return at >= 0 ? args[at + 1] : undefined;
};

const out = option('--out');
if (out === undefined) {
  console.error('usage: pack.mjs --out <dir> [--sdk <path to @luna/sdk src/provider.ts>]');
  process.exit(2);
}
const sdk = resolve(option('--sdk') ?? join(root, '..', '..', '..', 'sdk', 'typescript', 'src', 'provider.ts'));
if (!existsSync(sdk)) {
  console.error(`the @luna/sdk provider module was not found at ${sdk}; pass --sdk`);
  process.exit(2);
}

const target = resolve(out);
if (existsSync(target)) {
  if (readdirSync(target).length > 0 && !existsSync(join(target, 'luna-plugin.json'))) {
    console.error(`${target} is not empty and is not a package; refusing to replace it`);
    process.exit(2);
  }
  rmSync(target, { recursive: true, force: true });
}

const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
const files = [
  ['bin/worker', join(root, 'bin', 'worker')],
  ['src/worker.ts', join(root, 'src', 'worker.ts')],
  ['vendor/luna-sdk/provider.ts', sdk],
  ['luna-provider.json', join(root, 'luna-provider.json')],
  ['package.json', join(root, 'package.json')],
  ['tsconfig.json', join(root, 'tsconfig.json')],
];
const listed = [];
for (const [path, source] of files) {
  const bytes = readFileSync(source);
  const destination = join(target, path);
  mkdirSync(dirname(destination), { recursive: true });
  writeFileSync(destination, bytes);
  listed.push({ path, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
}
chmodSync(join(target, 'bin', 'worker'), 0o755);

const entries = Object.fromEntries(
  ['macos-aarch64', 'macos-x86_64', 'linux-aarch64', 'linux-x86_64'].map((platform) => [platform, 'bin/worker']),
);
const manifest = {
  manifestVersion: 1,
  id: ID,
  version: pkg.version,
  displayName: 'Gemini CLI',
  publisher: 'novnski',
  description: 'Runs Google Gemini CLI as a LUNA agent provider. Native code: it runs with your account access.',
  license: 'MIT',
  // runtime.limits needs extension API 1.1.
  api: { major: 1, minMinor: 1 },
  runtime: {
    kind: 'native',
    protocol: { major: 1, minMinor: 0 },
    entries,
    // Gemini CLI relaunches itself with a larger Node.js heap; one long turn
    // can use more than the default 1 GiB group budget and one CPU hour.
    limits: { memoryMiB: 4096, cpuSeconds: 14400 },
  },
  contributions: [{ id: ID, kind: 'provider', title: 'Gemini CLI' }],
  capabilities: [
    { id: 'luna.capability/provider.register', reason: 'adds Gemini CLI and its models to the model picker' },
    { id: 'luna.capability/process.spawn', reason: 'starts the gemini CLI for every message' },
    { id: 'luna.capability/network', reason: "Gemini CLI talks to Google's model service" },
  ],
  files: listed,
};
const bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
writeFileSync(join(target, 'luna-plugin.json'), bytes);
console.log(
  JSON.stringify({ dir: target, packageHash: createHash('sha256').update(bytes).digest('hex') }, null, 2),
);
