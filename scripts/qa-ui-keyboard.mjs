#!/usr/bin/env node
// Native evidence collector for UI keyboard activation, Escape and focus-ring
// QA (docs/qa/ui-keyboard-activation-cases.md). It never drives input: the
// operator presses keys in the running QA app between snapshots. It only
// reads the app's persisted UI settings, its engine over IPC, and captures
// its window. Every artifact is written once under target/qa/<run>.
//
//   preflight <run> [--external-capture]  identity, signature, capture check
//   snap <run> <label> [--engine] [--image <file>]
//                                         settled ui-settings.json + window PNG
//   flip <run> <before> <after> <key>...  exactly these settings toggled once
//   same <run> <before> <after>           no setting (and, with engine
//                                         snapshots, no chat/session) change
//   pixels <run> <before> <after> [--max-height N] [--expect-change]
//          [--region x0,y0,x1,y1] [--scale px-per-point] [--threshold N]
//                                         (region/scale for full-screen captures)
//                                         changed-pixel box stays inside one
//                                         control: focus does not move layout
//   seed <run>                            one disposable project + two chats
//                                         (Mutate only; nothing is ever sent)
//   summary <run>                         roll up recorded checks
//
// Identity comes from the QA overrides in docs/reference/qa-infrastructure.md:
// LUNA_DEV_APP_NAME, LUNA_DEV_BUNDLE_ID, LUNA_DEV_IPC_PORT, LUNA_DEV_DATA_DIR.
// The primary LUNA Dev identity (port 59777) is refused.
//
// Window capture uses `screencapture` only when this process already has
// Screen Recording permission; it never triggers the permission prompt.
// Without it, pass `--image <file>` (any format `sips` reads, e.g. a window
// capture saved by the operator's automation) and the script converts and
// stores that instead.
//
// `preflight --external-capture` makes the whole run observation-free: the
// script never calls osascript/JXA or screencapture (not even for window
// metadata). Every `snap` must then supply `--image`; dimensions come from
// that image, its scale defaults to 1, and `pixels --scale` overrides it.
// Process, signature, IPC and settings checks are unchanged.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const [mode, run, ...rest] = process.argv.slice(2);
const MODES = ['preflight', 'seed', 'snap', 'flip', 'same', 'pixels', 'summary'];
assert.ok(MODES.includes(mode), `usage: qa-ui-keyboard.mjs ${MODES.join('|')} target/qa/<run> ...`);
assert.ok(run, 'supply an evidence directory under target/qa');
const evidence = path.resolve(run);
assert.ok(evidence.startsWith(path.join(root, 'target/qa') + path.sep), 'evidence must be under target/qa');

const env = name => {
  const value = process.env[name];
  assert.ok(value, `${name} is required (use the QA identity, never the primary app)`);
  return value;
};
const appName = env('LUNA_DEV_APP_NAME');
const bundleId = env('LUNA_DEV_BUNDLE_ID');
const port = Number(env('LUNA_DEV_IPC_PORT'));
const dataDir = path.resolve(env('LUNA_DEV_DATA_DIR'));
assert.ok(Number.isInteger(port) && port !== 59777, 'refusing the primary LUNA Dev port');
assert.notEqual(appName, 'LUNA Dev', 'refusing the primary LUNA Dev bundle');
const app = path.join(root, 'target/luna-macos-dev', `${appName}.app`);
const settingsFile = path.join(dataDir, 'ui-settings.json');

const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const run_ = (cmd, args) => execFileSync(cmd, args, { encoding: 'utf8' }).trim();
function save(name, value) {
  fs.writeFileSync(path.join(evidence, name), JSON.stringify(value, null, 2) + '\n', { flag: 'wx' });
}
function load(name) {
  return JSON.parse(fs.readFileSync(path.join(evidence, name), 'utf8'));
}
function snapFile(label) {
  const match = fs.readdirSync(evidence).find(f => f.endsWith(`-${label}.json`) && /^\d{2}-/.test(f));
  assert.ok(match, `no snapshot labelled ${label}`);
  return match;
}
function record(check, result) {
  const file = path.join(evidence, 'checks.jsonl');
  fs.appendFileSync(file, JSON.stringify({ check, at: new Date().toISOString(), ...result }) + '\n');
}

// The one running app must be this exact signed QA bundle.
function identity() {
  execFileSync('codesign', ['--verify', '--deep', '--strict', app]);
  const executable = fs.realpathSync(path.join(app, 'Contents/MacOS/luna'));
  const plistId = run_('plutil', ['-extract', 'CFBundleIdentifier', 'raw', path.join(app, 'Contents/Info.plist')]);
  assert.equal(plistId, bundleId, 'bundle identifier does not match LUNA_DEV_BUNDLE_ID');
  const pid = run_('lsof', ['-t', '-nP', `-iTCP:${port}`, '-sTCP:LISTEN']);
  assert.match(pid, /^\d+$/, 'exactly one IPC listener is required');
  assert.equal(run_('ps', ['-p', pid, '-o', 'command=']), executable, 'listener is not the QA bundle');
  const luna = run_('ps', ['-axo', 'pid=,command=']).split('\n')
    .filter(line => /\/Contents\/MacOS\/(luna|drafts-fixture)$/.test(line.trim()));
  assert.equal(luna.length, 1, `exactly one Luna GUI may run:\n${luna.join('\n')}`);
  return { pid: Number(pid), executable };
}

// Largest normal-layer window of the app; CGWindowList needs no permission.
function windowOf(pid) {
  const script = `ObjC.import("CoreGraphics");
const raw = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, $.kCGNullWindowID);
JSON.stringify(ObjC.deepUnwrap(ObjC.castRefToObject(raw)).filter(w => w.kCGWindowOwnerPID === ${pid} && w.kCGWindowLayer === 0)
  .map(w => ({ number: w.kCGWindowNumber, bounds: w.kCGWindowBounds })));`;
  const windows = JSON.parse(run_('osascript', ['-l', 'JavaScript', '-e', script]));
  assert.ok(windows.length > 0, 'the QA app has no on-screen window');
  windows.sort((a, b) => b.bounds.Width * b.bounds.Height - a.bounds.Width * a.bounds.Height);
  return windows[0];
}

// Other apps' window titles are visible to CGWindowList only with Screen
// Recording permission, so this checks it without ever prompting.
function canCapture() {
  const script = `ObjC.import("CoreGraphics");
const l = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, $.kCGNullWindowID)));
String(l.some(w => w.kCGWindowLayer === 0 && w.kCGWindowOwnerPID !== ${process.pid} && w.kCGWindowName));`;
  return run_('osascript', ['-l', 'JavaScript', '-e', script]) === 'true';
}

// A supplied capture is consumed directly: no window lookup, no permission
// probe. `sips` only normalizes the file to the PNG form decodePng reads.
function externalCapture(file, image) {
  const source = path.resolve(image);
  assert.ok(fs.existsSync(source), `no capture at ${source}`);
  execFileSync('sips', ['-s', 'format', 'png', source, '--out', file], { stdio: 'ignore' });
  const png = decodePng(fs.readFileSync(file));
  return {
    source: `external:${path.basename(image)}`,
    sourceSha256: sha256(fs.readFileSync(source)),
    png: { width: png.width, height: png.height },
    scale: 1,
  };
}

function capture(pid, file, image) {
  if (image) return externalCapture(file, image);
  assert.ok(!externalOnly(), 'this run uses --external-capture; pass --image <file>');
  const window = windowOf(pid);
  assert.ok(canCapture(), 'Screen Recording permission is absent; pass --image <file> (never grant it here)');
  execFileSync('screencapture', ['-x', '-o', `-l${window.number}`, file]);
  const png = decodePng(fs.readFileSync(file));
  return { window, source: 'screencapture', png: { width: png.width, height: png.height }, scale: png.width / window.bounds.Width };
}

function externalOnly() {
  const manifest = path.join(evidence, 'manifest.json');
  return fs.existsSync(manifest) && JSON.parse(fs.readFileSync(manifest, 'utf8')).captureMode === 'external';
}

// Settings are written through a 400 ms debounce: wait until the file is
// unchanged for a full second before reading it.
async function settledSettings() {
  let last = null;
  let stableSince = Date.now();
  const deadline = Date.now() + 8000;
  for (;;) {
    const bytes = fs.existsSync(settingsFile) ? fs.readFileSync(settingsFile) : Buffer.from('{}');
    const digest = sha256(bytes);
    if (digest !== last) { last = digest; stableSince = Date.now(); }
    if (Date.now() - stableSince >= 1000) return { digest, value: JSON.parse(bytes.toString('utf8')) };
    assert.ok(Date.now() < deadline, 'ui-settings.json did not settle');
    await new Promise(resolve => setTimeout(resolve, 100));
  }
}

async function rpc(work) {
  const ws = new WebSocket(`ws://127.0.0.1:${port}`);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });
  let nextId = 1;
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timeout = setTimeout(() => reject(new Error(`${method} timed out`)), 15000);
    ws.addEventListener('message', function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timeout);
      ws.removeEventListener('message', receive);
      if (frame.err) return reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
      resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
    });
    ws.send(JSON.stringify({ id, method, params }));
  });
  try {
    return await work(call);
  } finally {
    ws.close();
  }
}

async function engineSnapshot() {
  const ws = new WebSocket(`ws://127.0.0.1:${port}`);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });
  let nextId = 1;
  const first = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timeout = setTimeout(() => reject(new Error(`${method} timed out`)), 15000);
    ws.addEventListener('message', function receive(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timeout);
      ws.removeEventListener('message', receive);
      if (frame.err) return reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
      resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
    });
    ws.send(JSON.stringify({ id, method, params }));
  });
  try {
    return { chats: await first('WatchChats'), sessions: await first('WatchSessions') };
  } finally {
    ws.close();
  }
}

// Timestamps and activity counters move on their own; everything else about a
// chat or session must be untouched by a cancelled dialog.
const VOLATILE = /(At|AtMs|_at|_ms|Seen|Activity|elapsed|heartbeat|revision)$/i;
function stable(items) {
  const byId = {};
  for (const item of Array.isArray(items) ? items : []) {
    byId[item.id ?? item.chatId ?? JSON.stringify(item)] = Object.fromEntries(
      Object.entries(item).filter(([key]) => !VOLATILE.test(key)));
  }
  return byId;
}

// Minimal PNG decoder (8-bit, non-interlaced RGB/RGBA: what screencapture writes).
function decodePng(bytes) {
  assert.equal(bytes.subarray(0, 8).toString('hex'), '89504e470d0a1a0a', 'not a PNG');
  let offset = 8, width = 0, height = 0, colorType = 0, depth = 0, interlace = 0;
  const idat = [];
  while (offset < bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString('ascii', offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0); height = data.readUInt32BE(4);
      depth = data[8]; colorType = data[9]; interlace = data[12];
    } else if (type === 'IDAT') idat.push(data);
    else if (type === 'IEND') break;
    offset += 12 + length;
  }
  assert.ok(depth === 8 && (colorType === 2 || colorType === 6) && interlace === 0,
    `unsupported PNG (depth ${depth}, color ${colorType}, interlace ${interlace})`);
  const channels = colorType === 6 ? 4 : 3;
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const pixels = Buffer.alloc(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? pixels[y * stride + x - channels] : 0;
      const b = y > 0 ? pixels[(y - 1) * stride + x] : 0;
      const c = x >= channels && y > 0 ? pixels[(y - 1) * stride + x - channels] : 0;
      let value = line[x];
      if (filter === 1) value += a;
      else if (filter === 2) value += b;
      else if (filter === 3) value += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
        value += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      pixels[y * stride + x] = value & 0xff;
    }
  }
  return { width, height, channels, pixels };
}

if (mode === 'preflight') {
  assert.ok(!fs.existsSync(evidence), 'a preflight cannot be replayed into an existing run');
  fs.mkdirSync(evidence, { recursive: true });
  const { pid, executable } = identity();
  const external = rest.includes('--external-capture');
  // External mode: no OS observation at all; captures arrive with each snap.
  const probe = external
    ? { mode: 'external', note: 'operator-supplied captures via snap --image; no OS window probe' }
    : canCapture()
      ? capture(pid, path.join(evidence, '00-preflight.png'))
      : { blocked: 'no Screen Recording permission; snapshots need --image', window: windowOf(pid) };
  save('manifest.json', {
    createdAt: new Date().toISOString(),
    captureMode: external ? 'external' : 'native',
    app, appName, bundleId, port, dataDir, pid, executable,
    head: run_('git', ['-C', root, 'rev-parse', 'HEAD']),
    uiDiffSha256: sha256(execFileSync('git', ['-C', root, 'diff', '--', 'crates/ui'])),
    settingsSha256: fs.existsSync(settingsFile) ? sha256(fs.readFileSync(settingsFile)) : null,
    capture: probe,
  });
  console.log(`preflight ok: ${appName} pid ${pid}; evidence ${evidence}`);
} else if (mode === 'seed') {
  assert.ok(fs.existsSync(path.join(evidence, 'manifest.json')), 'run preflight first');
  identity();
  const workspace = path.join(evidence, 'workspace');
  assert.ok(!fs.existsSync(workspace), 'a seed cannot be replayed');
  fs.mkdirSync(workspace, { recursive: true });
  execFileSync('git', ['init', '-q', workspace]);
  const seeded = await rpc(async call => {
    const info = await call('EngineInfo');
    const mutate = async params => {
      const result = await call('Mutate', params);
      let receipt = result?.receipt;
      const deadline = Date.now() + 15000;
      while (receipt?.status === 'pending' && Date.now() < deadline) {
        await new Promise(resolve => setTimeout(resolve, 100));
        receipt = await call('GetTaskCommandReceipt', { commandId: receipt.commandId });
      }
      if (receipt) assert.equal(receipt.status, 'applied', JSON.stringify(receipt));
    };
    const spaceId = crypto.randomUUID();
    await mutate({ op: 'createSpace', spaceId, deviceId: info.deviceId, path: workspace,
      name: 'UI keyboard QA', gitDetected: true });
    // Read-only, ask-first config; the QA never submits a turn.
    const config = { harness: 'codex', sandbox: 'read-only', permissionMode: 'ask',
      effectivePermissionMode: 'ask' };
    const chats = {};
    for (const name of ['QA alpha', 'QA beta']) {
      const chatId = crypto.randomUUID();
      await mutate({ op: 'createChat', chatId, spaceId, cwd: workspace, config });
      await mutate({ op: 'renameChat', chatId, title: name });
      chats[name] = chatId;
    }
    return { spaceId, workspace, chats };
  });
  save('seed.json', seeded);
  console.log(`seeded project ${seeded.spaceId} with ${Object.keys(seeded.chats).join(', ')}`);
} else if (mode === 'snap') {
  const [label] = rest;
  const flag = rest.includes('--engine') ? '--engine' : null;
  const imageAt = rest.indexOf('--image');
  const image = imageAt >= 0 ? rest[imageAt + 1] : null;
  assert.match(label ?? '', /^[a-z0-9-]+$/, 'supply a snapshot label');
  assert.ok(fs.existsSync(path.join(evidence, 'manifest.json')), 'run preflight first');
  const { pid } = identity();
  const index = String(fs.readdirSync(evidence).filter(f => /^\d{2}-.*\.json$/.test(f)).length + 1).padStart(2, '0');
  const settings = await settledSettings();
  const png = `${index}-${label}.png`;
  const shot = capture(pid, path.join(evidence, png), image);
  save(`${index}-${label}.json`, {
    label, at: new Date().toISOString(), pid, png,
    pngSha256: sha256(fs.readFileSync(path.join(evidence, png))),
    capture: shot, settings,
    engine: flag === '--engine' ? await engineSnapshot() : null,
  });
  console.log(`snapshot ${index}-${label}`);
} else if (mode === 'flip' || mode === 'same') {
  const [before, after, ...keys] = rest;
  const a = load(snapFile(before)), b = load(snapFile(after));
  const changed = Object.keys({ ...a.settings.value, ...b.settings.value })
    .filter(key => JSON.stringify(a.settings.value[key]) !== JSON.stringify(b.settings.value[key]));
  const expected = mode === 'flip' ? keys : [];
  if (mode === 'flip') {
    assert.ok(keys.length > 0, 'name the setting keys that must flip');
    for (const key of keys) {
      assert.equal(typeof a.settings.value[key], 'boolean', `${key} is not a boolean setting`);
      assert.equal(b.settings.value[key], !a.settings.value[key], `${key} did not flip exactly once`);
    }
  }
  assert.deepEqual(changed.sort(), [...expected].sort(), `unexpected setting changes: ${changed.join(', ')}`);
  let engine = null;
  if (a.engine && b.engine) {
    assert.deepEqual(stable(b.engine.chats), stable(a.engine.chats), 'a chat changed');
    assert.deepEqual(stable(b.engine.sessions), stable(a.engine.sessions), 'a session changed');
    engine = 'unchanged';
  }
  record(mode, { before, after, keys: expected, changed, engine, pass: true });
  console.log(`${mode} ok: ${before} -> ${after} changed [${changed.join(', ')}]${engine ? ', engine unchanged' : ''}`);
} else if (mode === 'pixels') {
  const [before, after, ...flags] = rest;
  const maxHeight = Number(flags[flags.indexOf('--max-height') + 1] || 44);
  const expectChange = flags.includes('--expect-change');
  const flag = name => (flags.includes(name) ? flags[flags.indexOf(name) + 1] : null);
  const region = flag('--region')?.split(',').map(Number);
  // JPEG captures (the --image path) spread a change by up to one 16 px
  // block; a higher threshold keeps that spill out of the box.
  const threshold = Number(flag('--threshold')) || 8;
  const a = load(snapFile(before)), b = load(snapFile(after));
  const pa = decodePng(fs.readFileSync(path.join(evidence, a.png)));
  const pb = decodePng(fs.readFileSync(path.join(evidence, b.png)));
  assert.ok(pa.width === pb.width && pa.height === pb.height, 'window size changed between snapshots');
  let minX = Infinity, minY = Infinity, maxX = -1, maxY = -1, count = 0;
  const [x0, y0, x1, y1] = region ?? [0, 0, pa.width, pa.height];
  for (let y = Math.max(0, y0); y < Math.min(pa.height, y1); y++) {
    for (let x = Math.max(0, x0); x < Math.min(pa.width, x1); x++) {
      const i = (y * pa.width + x) * pa.channels, j = (y * pb.width + x) * pb.channels;
      if (Math.abs(pa.pixels[i] - pb.pixels[j]) > threshold || Math.abs(pa.pixels[i + 1] - pb.pixels[j + 1]) > threshold
        || Math.abs(pa.pixels[i + 2] - pb.pixels[j + 2]) > threshold) {
        count++; minX = Math.min(minX, x); minY = Math.min(minY, y);
        maxX = Math.max(maxX, x); maxY = Math.max(maxY, y);
      }
    }
  }
  const scale = Number(flag('--scale')) || b.capture.scale;
  const box = count === 0 ? null : {
    x: minX / scale, y: minY / scale, width: (maxX - minX + 1) / scale, height: (maxY - minY + 1) / scale,
  };
  const failure = expectChange && !box
    ? 'expected a visible focus ring, saw no change'
    : box && box.height > maxHeight
      ? `changed region is ${box.height.toFixed(1)}pt tall (> ${maxHeight}pt): layout moved`
      : null;
  // Failures are evidence too: record before asserting.
  record('pixels', { before, after, region: region ?? null, scale, threshold, changedPixels: count, boxPoints: box, maxHeight, pass: !failure, failure });
  assert.ok(!failure, failure);
  console.log(`pixels ok: ${count} px changed${box ? `, box ${box.width.toFixed(1)}x${box.height.toFixed(1)}pt at ${box.x.toFixed(1)},${box.y.toFixed(1)}` : ''}`);
} else if (mode === 'summary') {
  const file = path.join(evidence, 'checks.jsonl');
  const checks = fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trim().split('\n').map(JSON.parse) : [];
  const artifacts = Object.fromEntries(fs.readdirSync(evidence)
    .filter(f => f !== 'summary.json' && fs.statSync(path.join(evidence, f)).isFile())
    .map(f => [f, sha256(fs.readFileSync(path.join(evidence, f)))]));
  save('summary.json', { manifest: load('manifest.json'), checks, artifacts });
  console.log(`summary: ${checks.length} checks, ${Object.keys(artifacts).length} artifacts`);
}
