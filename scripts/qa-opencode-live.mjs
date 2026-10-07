#!/usr/bin/env node
// OpenCode QA live driver (OC-01): signed disposable LUNA QA app -> real
// OpenCode (T2 mock provider or T3 real model) -> evidence per
// docs/reference/qa-infrastructure.md "OpenCode QA" and validation-plan §2.
//
//   node scripts/qa-opencode-live.mjs run --tier T2|T3 [--provider opencode]
//        [--model <provider/model>] [--variant <level>] [--scenario <name>]
//        [--prompt <text>]... [--prompt-file <path>]... [--permission-mode full]
//        [--expect-status idle|errored] [--turn-timeout <seconds>]
//        [--mock-url <url>] [--mock-log <path>] [--evidence-root <dir>]
//   node scripts/qa-opencode-live.mjs snapshot [--run <dir>]
//   node scripts/qa-opencode-live.mjs verify-restart [--run <dir>]
//
// Identity is asserted before any write. Every send is recorded in
// manifest.json before its RPC; a failed or unknown send is never replayed.
// `snapshot` and `verify-restart` never send. --provider claude-code|codex runs
// the same steps for PD-14 comparison diffs.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync, spawnSync} from 'node:child_process';
import {parseArgs} from 'node:util';

const root = path.resolve(import.meta.dirname, '..');
const mode = process.argv[2];
const {values: opt} = parseArgs({
  args: process.argv.slice(3),
  options: {
    provider: {type: 'string', default: 'opencode'},
    model: {type: 'string'},
    variant: {type: 'string'},
    scenario: {type: 'string', default: 'oce-adhoc'},
    prompt: {type: 'string', multiple: true},
    'prompt-file': {type: 'string', multiple: true},
    tier: {type: 'string'},
    'permission-mode': {type: 'string', default: 'full'},
    'turn-timeout': {type: 'string', default: '240'},
    'expect-status': {type: 'string', default: 'idle'},
    'mock-url': {type: 'string'},
    'mock-log': {type: 'string'},
    'evidence-root': {type: 'string', default: path.join(root, 'target/qa/opencode')},
    run: {type: 'string'},
  },
});
if (!['run', 'snapshot', 'verify-restart'].includes(mode)) {
  console.error('usage: qa-opencode-live.mjs run|snapshot|verify-restart [options] (see header)');
  process.exit(64);
}
const provider = opt.provider;
assert.ok(['opencode', 'claude-code', 'codex'].includes(provider), `unsupported provider ${provider}`);
const isOpencode = provider === 'opencode';

// ------------------------------------------------------------------ identity
// The OpenCode QA identity from docs/reference/qa-infrastructure.md; the
// LUNA_DEV_* variables may point the driver at another QA worktree identity.
const identity = {
  appName: process.env.LUNA_DEV_APP_NAME ?? 'LUNA QA OpenCode',
  bundleId: process.env.LUNA_DEV_BUNDLE_ID ?? 'com.novnski.luna.qa.opencode',
  port: Number(process.env.LUNA_DEV_IPC_PORT ?? 59840),
  dataDir: path.resolve(process.env.LUNA_DEV_DATA_DIR ?? path.join(root, 'target/luna-qa-data/opencode')),
};
// QF-09: never drive the primary app.
assert.notEqual(identity.port, 59777, 'refusing the primary LUNA Dev IPC port');
assert.notEqual(identity.bundleId, 'com.novnski.luna.dev', 'refusing the primary LUNA Dev bundle');
assert.match(identity.bundleId, /^com\.novnski\.luna\.qa\.[a-z][a-z0-9-]*$/);
const bundle = path.join(root, 'target/luna-macos-dev', `${identity.appName}.app`);
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'}));
const lsenv = plist.LSEnvironment ?? {};
assert.equal(plist.CFBundleIdentifier, identity.bundleId, 'bundle id mismatch');
assert.equal(lsenv.LUNA_IPC_PORT, String(identity.port), 'plist IPC port mismatch');
assert.equal(path.resolve(lsenv.LUNA_DATA_DIR ?? ''), identity.dataDir, 'plist data dir mismatch');
assert.equal(lsenv.LUNA_QA_MCP_ISOLATION, '1', 'QA bundle must isolate user MCP servers');
const inside = (child, parent) => {
  const rel = path.relative(parent, child);
  return rel !== '' && !rel.startsWith('..') && !path.isAbsolute(rel);
};
let profile = null;
if (isOpencode) {
  const exe = lsenv.OPENCODE_EXECUTABLE;
  assert.ok(exe, 'plist lacks OPENCODE_EXECUTABLE (set LUNA_DEV_OPENCODE_EXECUTABLE before refreshing the QA app)');
  assert.ok(inside(path.resolve(exe), identity.dataDir), 'OPENCODE_EXECUTABLE must live inside the QA data dir');
  assert.ok(fs.readFileSync(exe, 'utf8').includes('# luna-qa-opencode-profile'), 'OPENCODE_EXECUTABLE is not a QA profile wrapper');
  const profileDir = path.dirname(path.dirname(exe));
  profile = {dir: profileDir, wrapper: exe, ...JSON.parse(fs.readFileSync(path.join(profileDir, 'luna-qa-opencode-profile.json'), 'utf8'))};
}
const listeners = [...new Set(execFileSync('lsof', ['-nP', `-iTCP:${identity.port}`, '-sTCP:LISTEN', '-Fp'], {encoding: 'utf8'}).match(/^p\d+$/gm) ?? [])];
assert.equal(listeners.length, 1, `expected one listener on ${identity.port}`);
const pid = Number(listeners[0].slice(1));
const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
assert.equal(execFileSync('ps', ['-p', String(pid), '-o', 'comm='], {encoding: 'utf8'}).trim(), executable, 'listener is not the QA bundle executable');

// ------------------------------------------------------------------ evidence
const scenarioRoot = path.join(path.resolve(opt['evidence-root']), opt.scenario);
let evidence;
const save = (name, value) => {
  const file = path.join(evidence, name);
  fs.mkdirSync(path.dirname(file), {recursive: true});
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
};
const append = (name, value) => fs.appendFileSync(path.join(evidence, name), `${JSON.stringify(value)}\n`);
const now = () => new Date().toISOString();
const sha256 = data => crypto.createHash('sha256').update(data).digest('hex');
const git = (...args) => spawnSync('git', ['-C', root, ...args], {encoding: 'utf8'}).stdout.trim();
const command = (name, argv, extra = {}) => append('commands.jsonl', {name, argv, at: now(), ...extra});

function wireOffsets(dir) {
  if (!dir || !fs.existsSync(dir)) return {dir: dir ?? null, present: false, files: {}};
  const files = {};
  for (const name of fs.readdirSync(dir)) {
    const file = path.join(dir, name);
    if (fs.statSync(file).isFile()) files[name] = fs.statSync(file).size;
  }
  return {dir, present: true, files};
}

function sliceFile(source, start, destination) {
  const size = fs.statSync(source).size;
  const length = Math.max(0, size - start);
  const buffer = Buffer.alloc(length);
  const fd = fs.openSync(source, 'r');
  try { fs.readSync(fd, buffer, 0, length, start); } finally { fs.closeSync(fd); }
  fs.mkdirSync(path.dirname(destination), {recursive: true});
  fs.writeFileSync(destination, buffer);
  return {startOffset: start, endOffset: size, bytes: length, sha256: sha256(buffer)};
}

// ------------------------------------------------------------------ RPC
const ws = new WebSocket(`ws://127.0.0.1:${identity.port}`);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
const listenersById = new Map();
ws.addEventListener('message', event => {
  for (const line of String(event.data).split('\n').filter(Boolean)) {
    let frame;
    try { frame = JSON.parse(line); } catch { continue; }
    listenersById.get(frame.id)?.(frame);
  }
});
function call(method, params = {}, timeoutMs = 45000) {
  return new Promise((resolve, reject) => {
    const id = ++seq;
    const timer = setTimeout(() => {
      listenersById.delete(id);
      ws.send(JSON.stringify({id, cancel: true}));
      reject(Object.assign(new Error(`${method} timed out; outcome unknown, never replayed`), {unknown: true}));
    }, timeoutMs);
    listenersById.set(id, frame => {
      if (!['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) return;
      clearTimeout(timer);
      listenersById.delete(id);
      if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({id, cancel: true}));
      if (frame.err) reject(new Error(JSON.stringify(frame.err)));
      else resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
    });
    ws.send(JSON.stringify({id, method, params}));
  });
}
// Persistent subscriptions only (validation-plan §2 item 6): no polling.
const subscriptions = [];
function subscribe(method, params, onItem) {
  const id = ++seq;
  const record = {method, params, id, subscribedAt: now(), frames: 0, firstFrameAt: null, lastFrameAt: null, closedAt: null, error: null};
  subscriptions.push(record);
  listenersById.set(id, frame => {
    if (Object.hasOwn(frame, 'err')) { record.error = frame.err; return; }
    if (!Object.hasOwn(frame, 'item')) return;
    record.frames += 1;
    record.firstFrameAt ??= now();
    record.lastFrameAt = now();
    onItem(frame.item);
  });
  ws.send(JSON.stringify({id, method, params}));
  return () => {
    if (record.closedAt) return;
    record.closedAt = now();
    listenersById.delete(id);
    ws.send(JSON.stringify({id, cancel: true}));
  };
}
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const entriesOf = value => value?.frame?.reset ?? value?.reset ?? (Array.isArray(value) ? value : []);

async function readState(chatId) {
  const [identityView, work] = await Promise.all([
    call('GetTaskIdentity', {taskId: chatId}).catch(error => ({error: String(error)})),
    call('GetTaskWork', {taskId: chatId, limit: 100}).catch(error => ({error: String(error)})),
  ]);
  const transcript = entriesOf(await call('WatchDocMessages', {chatId}).catch(() => null));
  const queue = await call('WatchQueue', {chatId}).catch(error => ({error: String(error)}));
  return {at: now(), pid, identity: identityView, work, queue, transcript};
}

function finishEvidence(manifest, criteria, extraSecrets = []) {
  manifest.criteria = criteria;
  manifest.passed = criteria.every(c => c.passed);
  manifest.endedAt = now();
  save('manifest.json', manifest);
  const scanArgs = [path.join(root, 'scripts/qa-opencode-manifest.py'), 'scan', '--dir', evidence, ...extraSecrets.flatMap(name => ['--secret-env', name])];
  const scan = spawnSync('python3', scanArgs, {encoding: 'utf8'});
  command('secret-scan', ['python3', ...scanArgs], {exit: scan.status});
  criteria.push({criterion: 'secret-scan', expected: 'no credentials in evidence (OCF-142)', actual: scan.stdout.trim(), passed: scan.status === 0});
  manifest.criteria = criteria;
  manifest.passed = criteria.every(c => c.passed);
  save('manifest.json', manifest);
  const rows = criteria.map(c => `| ${c.criterion} | ${c.expected} | ${String(typeof c.actual === 'string' ? c.actual : JSON.stringify(c.actual)).replaceAll('|', '/').slice(0, 300)} | ${c.passed ? 'yes' : 'NO'} |`);
  fs.writeFileSync(path.join(evidence, 'result.md'), [
    `# OpenCode QA live run: ${manifest.scenario} (${manifest.mode})`, '',
    `- Provider: ${manifest.requested.provider}; requested model ${manifest.requested.model ?? '(default)'}; variant ${manifest.requested.variant ?? '(none)'}.`,
    `- Evidence tier: **${manifest.tier}** through the signed QA app's RPC surface (engine, document, adapter). Not UI evidence: no screenshots or AX readback are taken by this driver.`,
    `- App ${identity.appName} pid ${pid}; LUNA ${manifest.luna.sha} (dirty=${manifest.luna.dirty}).`,
    `- Result: **${manifest.passed ? 'PASS' : 'FAIL'}**`, '',
    '| Criterion | Expected | Actual | Pass |', '| --- | --- | --- | --- |', ...rows, '',
    '## Unverified by this driver', '',
    '- Native server readback (session record, message pages, pending asks) beyond what the wire log captured: the server password stays private to LUNA.',
    '- Model consumption claims; a model statement is not verification (validation-plan §1 item 7).', '',
    '## Replay', '', '```', manifest.replay, '```', '',
  ].join('\n'));
  if (manifest.passed) save('pass.json', {scenario: manifest.scenario, tier: manifest.tier, mode: manifest.mode, passedAt: now()});
  spawnSync('python3', [path.join(root, 'scripts/qa-opencode-manifest.py'), 'sums', '--dir', evidence], {encoding: 'utf8'});
  return manifest.passed;
}

const criteria = [];
const check = (criterion, expected, actual, passed) => {
  criteria.push({criterion, expected, actual, passed: Boolean(passed)});
  console.error(`[${passed ? 'PASS' : 'FAIL'}] ${criterion}: ${typeof actual === 'string' ? actual : JSON.stringify(actual)}`);
};

try {
  if (mode === 'run') {
    assert.ok(['T2', 'T3'].includes(opt.tier), '--tier T2 (mock provider) or T3 (real model) is required (QF-05)');
    const prompts = [...(opt.prompt ?? []), ...(opt['prompt-file'] ?? []).map(file => fs.readFileSync(file, 'utf8'))];
    if (prompts.length === 0) prompts.push(opt.tier === 'T2' ? '[[mock:text]] OpenCode QA live run: reply with the scripted text.' : 'Reply with exactly the word READY and nothing else. Do not use tools.');
    fs.mkdirSync(scenarioRoot, {recursive: true});
    evidence = fs.mkdtempSync(path.join(scenarioRoot, `run-${new Date().toISOString().replace(/[-:.]/g, '').slice(0, 15)}Z-`));
    fs.writeFileSync(path.join(scenarioRoot, 'latest'), `${evidence}\n`);
    console.error(`evidence: ${evidence}`);

    // QF-02: the fixture lives under the system temp dir, never inside $HOME.
    const fixture = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'luna-qa-opencode-live-')), 'fixture');
    assert.ok(!inside(fs.realpathSync(path.dirname(fixture)), fs.realpathSync(os.homedir())), 'fixture must be outside $HOME (QF-02)');
    fs.mkdirSync(fixture);
    for (const args of [['init', '-q', '-b', 'main', fixture], ['-C', fixture, 'config', 'user.name', 'OpenCode QA'], ['-C', fixture, 'config', 'user.email', 'qa@example.invalid']]) execFileSync('git', args);
    fs.writeFileSync(path.join(fixture, 'README.md'), 'Disposable OpenCode QA fixture.\n');
    execFileSync('git', ['-C', fixture, 'add', 'README.md']);
    execFileSync('git', ['-C', fixture, 'commit', '-q', '-m', 'fixture']);

    const wireDir = lsenv.LUNA_OPENCODE_WIRE_LOG ?? null;
    const manifest = {
      schema: 'luna-opencode-qa-manifest/1', mode, scenario: opt.scenario, tier: opt.tier, startedAt: now(),
      luna: {sha: git('rev-parse', 'HEAD'), branch: git('rev-parse', '--abbrev-ref', 'HEAD'), dirty: git('status', '--porcelain') !== ''},
      app: {name: identity.appName, bundleId: identity.bundleId, port: identity.port, pid, executable, dataDir: identity.dataDir, codesign: 'verified (--deep --strict)', mcpIsolation: lsenv.LUNA_QA_MCP_ISOLATION},
      requested: {provider, model: opt.model ?? null, variant: opt.variant ?? null, permissionMode: opt['permission-mode']},
      fixture, xdg: profile ? {isolated: true, profile: profile.dir, homeIsolated: profile.homeIsolated, offline: profile.offline, projectConfigDisabled: profile.projectConfigDisabled} : null,
      server: isOpencode ? {ownership: 'spawned by the LUNA adapter', credentials: 'LUNA-private; never captured'} : null,
      wireLog: isOpencode ? {...wireOffsets(wireDir), start: null} : null,
      sends: [], tasks: {},
      replay: ['node', 'scripts/qa-opencode-live.mjs', ...process.argv.slice(2)].join(' '),
    };
    if (isOpencode) {
      const out = path.join(evidence, 'binary-resolution.json');
      const argv = [path.join(root, 'scripts/qa-opencode-manifest.py'), 'binary', '--path', profile.wrapper, '--out', out];
      const record = spawnSync('python3', argv, {encoding: 'utf8'});
      command('binary-record', ['python3', ...argv], {exit: record.status});
      manifest.opencodeBinary = fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, 'utf8')) : {error: record.stderr};
      manifest.wireLog.start = manifest.wireLog.files;
    }
    let mockBefore = null;
    if (opt['mock-url']) mockBefore = await (await fetch(`${opt['mock-url']}/__mock/state`)).json();
    const mockLogStart = opt['mock-log'] && fs.existsSync(opt['mock-log']) ? fs.statSync(opt['mock-log']).size : 0;
    save('manifest.json', manifest);

    const engine = await call('EngineInfo');
    save('engine-info.json', engine);
    const catalog = await call('ListModels', {harness: provider});
    save('catalog.json', catalog);
    const choose = () => {
      if (opt.model) return catalog.find(m => m.id === opt.model);
      if (isOpencode && opt.tier === 'T2') return catalog.find(m => m.id.startsWith('luna-mock/'));
      if (isOpencode) return catalog.find(m => /^opencode\/.*(-free|big-pickle)$/.test(m.id)) ?? catalog.find(m => m.isDefault);
      return catalog.find(m => m.isDefault) ?? catalog[0];
    };
    const model = choose();
    check('model-in-catalog', 'requested or tier default model listed by ListModels', model?.id ?? null, Boolean(model));
    assert.ok(model, 'no usable model in the LUNA catalog');
    // QF-05: tier labelling follows the effective model.
    const mockModel = model.id.startsWith('luna-mock/');
    check('tier-label', opt.tier === 'T2' ? 'T2 uses the luna-mock provider' : 'T3 uses a real (non-mock) model', model.id, opt.tier === 'T2' ? mockModel : !mockModel);
    manifest.requested.model = model.id;
    const sandbox = opt['permission-mode'] === 'full' ? 'danger-full-access' : 'workspace-write';
    const config = {harness: provider, model: model.id, reasoning: opt.variant ?? null, permissionMode: opt['permission-mode'], effectivePermissionMode: opt['permission-mode'], sandbox};

    const projectId = crypto.randomUUID();
    await call('Mutate', {op: 'createSpace', spaceId: projectId, deviceId: engine.deviceId, path: fixture});
    const chatId = crypto.randomUUID();
    manifest.tasks[provider] = chatId;
    manifest.projectId = projectId;
    save('manifest.json', manifest);
    const {snapshot} = await call('GetTaskCommandSnapshot', {taskId: chatId});
    const created = await call('ExecuteTaskCommand', {taskId: chatId, commandId: crypto.randomUUID(), actorId: 'qa-user', clientId: 'qa-opencode', clientSessionId: 'qa-opencode', expectedTaskRevision: snapshot.taskRevision, expectedBindingGeneration: snapshot.bindingGeneration, intent: {kind: 'createChat', spaceId: projectId, cwd: fixture, config, title: `OpenCode QA ${opt.scenario}`}});
    check('chat-created', 'createChat applied', created.status, created.status === 'applied');
    save('state-before.json', await readState(chatId));

    let lastSession = null;
    const sessionsSeen = [];
    const closers = [
      // TranscriptUpdate: a reset first, then deltas; full transcripts come
      // from the milestone reads (readState), so only summarize here.
      subscribe('WatchDocMessages', {chatId}, item => {
        const frame = item?.frame ?? item ?? {};
        const summary = frame.reset
          ? {kind: 'reset', entries: frame.reset.length, ids: frame.reset.map(e => e.id)}
          : {kind: 'delta', count: frame.count, upsert: frame.upsert?.length ?? 0, append: frame.append?.length ?? 0, remove: frame.remove?.length ?? 0};
        append('normalized/doc-frames.jsonl', {at: now(), ...summary, providerNotice: item?.providerNotice ?? null, contextUsage: item?.contextUsage ?? null});
      }),
      subscribe('WatchQueue', {chatId}, item => append('normalized/queue-frames.jsonl', {at: now(), item})),
      subscribe('WatchSessions', {}, item => {
        const rows = (Array.isArray(item) ? item : item?.sessions ?? []).filter(s => s.chatId === chatId);
        for (const row of rows) { lastSession = row; sessionsSeen.push({at: now(), status: row.status, lastCompletedTurn: row.lastCompletedTurn ?? null}); }
        if (rows.length) append('normalized/session-frames.jsonl', {at: now(), rows});
      }),
    ];

    const turnTimeout = Number(opt['turn-timeout']) * 1000;
    for (const [index, text] of prompts.entries()) {
      const messageId = crypto.randomUUID();
      const send = {index, chatId, messageId, promptSha256: sha256(text), prompt: text, attemptedAt: now(), outcome: 'pending'};
      manifest.sends.push(send);
      save('manifest.json', manifest); // QF-10: recorded before the RPC
      const sawBefore = sessionsSeen.length;
      const completedBefore = lastSession?.lastCompletedTurn ?? null;
      try {
        send.response = await call('QueueMessage', {chatId, messageId, text, holdForTurnEnd: true});
        send.outcome = 'admitted';
      } catch (error) {
        send.outcome = error.unknown ? 'unknown' : 'rejected';
        send.error = String(error);
        save('manifest.json', manifest);
        throw new Error(`send ${index} ${send.outcome}; not replayed: ${error}`);
      }
      save('manifest.json', manifest);
      const deadline = Date.now() + turnTimeout;
      let working = false;
      while (Date.now() < deadline) {
        const recent = sessionsSeen.slice(sawBefore);
        working ||= recent.some(s => s.status === 'working');
        // lastCompletedTurn survives coalesced frames that skip `working`.
        const advanced = Boolean(lastSession?.lastCompletedTurn) && lastSession.lastCompletedTurn !== completedBefore;
        const terminal = lastSession && ['idle', 'errored'].includes(lastSession.status);
        if (terminal && (working || advanced)) break;
        await delay(250); // waits on subscription frames; no RPC polling
      }
      send.settledStatus = lastSession?.status ?? null;
      send.settledAt = now();
      save('manifest.json', manifest);
      const expected = opt['expect-status'];
      check(`turn-${index}-settled`, `session reached ${expected} within the turn timeout`, `${working ? 'working->' : ''}${send.settledStatus}`, send.settledStatus === expected);
      save(`state-after-turn-${index}.json`, await readState(chatId));
    }
    await delay(2000);
    const after = await readState(chatId);
    save('state-after.json', after);
    for (const send of manifest.sends) {
      const count = after.transcript.filter(entry => entry.id === send.messageId).length;
      check(`send-${send.index}-once`, 'the user message appears exactly once in the transcript', count, count === 1);
    }
    for (const close of closers) close();
    save('observer.json', {subscriptions, policy: 'persistent WatchDocMessages/WatchQueue/WatchSessions; snapshots via single reads at milestones', sessionsSeen});
    manifest.native = {sessionId: after.identity?.currentBinding?.providerTaskId ?? null, readback: isOpencode ? 'wire log only (LUNA-private server)' : 'n/a'};

    if (isOpencode) {
      // QF-11: the run's slice of the adapter wire log, never fabricated.
      const end = wireOffsets(wireDir);
      const slices = {};
      if (end.present) {
        for (const [name, size] of Object.entries(end.files)) {
          const start = manifest.wireLog.start?.[name] ?? 0;
          if (size > start) slices[name] = sliceFile(path.join(wireDir, name), start, path.join(evidence, 'wire', name));
        }
      }
      for (const name of ['native-frames.jsonl', 'http.jsonl']) if (slices[name]) fs.copyFileSync(path.join(evidence, 'wire', name), path.join(evidence, name));
      manifest.wireLog = {...manifest.wireLog, end: end.files, slices};
      const captured = Object.values(slices).reduce((sum, s) => sum + s.bytes, 0);
      check('wire-log-captured', 'LUNA_OPENCODE_WIRE_LOG present with bytes for this run (QF-11)', `${wireDir ?? 'unset'}; ${captured} bytes`, end.present && captured > 0);
    }
    if (opt['mock-url']) {
      const mockAfter = await (await fetch(`${opt['mock-url']}/__mock/state`)).json();
      manifest.mock = {url: opt['mock-url'], before: mockBefore, after: mockAfter};
      const requests = Object.values(mockAfter.counts ?? {}).reduce((a, b) => a + b, 0) - Object.values(mockBefore?.counts ?? {}).reduce((a, b) => a + b, 0);
      check('mock-requests', opt.tier === 'T2' ? 'the mock served this run' : 'the mock was not used by a T3 run', requests, opt.tier === 'T2' ? requests > 0 : requests === 0);
    } else if (opt.tier === 'T2') {
      check('mock-requests', 'T2 needs --mock-url to prove the mock served the run', 'no --mock-url', false);
    }
    if (opt['mock-log'] && fs.existsSync(opt['mock-log'])) manifest.mockLog = sliceFile(opt['mock-log'], mockLogStart, path.join(evidence, 'mock-requests.jsonl'));
    manifest.completed = true;
    const passed = finishEvidence(manifest, criteria);
    console.log(JSON.stringify({passed, evidence, chatId}));
    process.exitCode = passed ? 0 : 1;
  } else {
    evidence = path.resolve(opt.run ?? fs.readFileSync(path.join(scenarioRoot, 'latest'), 'utf8').trim());
    assert.ok(fs.existsSync(path.join(evidence, 'manifest.json')), `no manifest in ${evidence}`);
    const manifest = JSON.parse(fs.readFileSync(path.join(evidence, 'manifest.json'), 'utf8'));
    const chatId = manifest.tasks?.[manifest.requested?.provider];
    assert.ok(chatId, 'manifest has no chat to read');
    const state = await readState(chatId);
    if (mode === 'snapshot') {
      save(`snapshot-${Date.now()}.json`, state);
      console.log(JSON.stringify({evidence, pid, entries: state.transcript.length}));
    } else {
      assert.equal(manifest.completed, true, 'verify-restart needs a completed run');
      const restart = {mode, at: now(), pid, previousPid: manifest.app?.pid, criteria: []};
      const local = [];
      const record = (criterion, expected, actual, passed) => { local.push({criterion, expected, actual, passed: Boolean(passed)}); console.error(`[${passed ? 'PASS' : 'FAIL'}] ${criterion}: ${JSON.stringify(actual)}`); };
      record('app-restarted', 'a different app pid than the run', `${manifest.app?.pid} -> ${pid}`, pid !== manifest.app?.pid);
      for (const send of manifest.sends ?? []) {
        const count = state.transcript.filter(entry => entry.id === send.messageId).length;
        record(`restart-send-${send.index}-once`, 'the user message survives restart exactly once', count, count === 1);
      }
      const pending = JSON.stringify(state.queue ?? {});
      record('restart-no-replayed-queue', 'no queue row for a recorded send after restart', (manifest.sends ?? []).filter(s => pending.includes(s.messageId)).length, !(manifest.sends ?? []).some(s => pending.includes(s.messageId)));
      restart.criteria = local;
      restart.state = state;
      save(`restart-${Date.now()}.json`, restart);
      manifest.restarts = [...(manifest.restarts ?? []), {at: restart.at, pid, passed: local.every(c => c.passed)}];
      save('manifest.json', manifest);
      spawnSync('python3', [path.join(root, 'scripts/qa-opencode-manifest.py'), 'sums', '--dir', evidence], {encoding: 'utf8'});
      console.log(JSON.stringify({passed: local.every(c => c.passed), evidence, restarted: true, pid}));
      process.exitCode = local.every(c => c.passed) ? 0 : 1;
    }
  }
} catch (error) {
  if (evidence) {
    save(`failure-${mode}-${Date.now()}.json`, {message: String(error), stack: error?.stack, time: now()});
  }
  throw error;
} finally {
  ws.close();
}
