#!/usr/bin/env node
// OpenCode QA live driver (OC-01): signed disposable LUNA QA app -> real
// OpenCode (T2 mock provider or T3 real model) -> evidence per
// docs/reference/qa-infrastructure.md "OpenCode QA" and validation-plan §2.
// Named multi-step scenarios S1..S8 live in scripts/qa-opencode-scenarios.mjs;
// this driver runs ad-hoc prompt lists and operator helpers.
//
//   node scripts/qa-opencode-live.mjs run --tier T2|T3 [--provider opencode]
//        [--model <provider/model>] [--variant <level>] [--scenario <name>]
//        [--prompt <text>]... [--prompt-file <path>]... [--permission-mode full|ask]
//        [--answer none|allow-once|deny|cancel] [--pick <option text>]
//        [--capture [--ax]] [--expect-status idle|errored] [--turn-timeout <seconds>]
//        [--mock-url <url>] [--mock-log <path>] [--evidence-root <dir>]
//   node scripts/qa-opencode-live.mjs snapshot [--run <dir>]
//   node scripts/qa-opencode-live.mjs verify-restart [--run <dir>]
//   node scripts/qa-opencode-live.mjs answer --run <dir> --action allow-once|deny|cancel|pick:<text>|text:<text>|dismiss|mcp:Accept|mcp:Decline|mcp:Cancel
//   node scripts/qa-opencode-live.mjs capture [--run <dir>] [--label <name>] [--ax]
//   node scripts/qa-opencode-live.mjs rehearse [run options]   (offline fake engine, tier R0, QF-21)
//
// Identity is asserted before any write (QF-09). Every send is recorded in
// manifest.json before its RPC; a failed or unknown send is never replayed
// (QF-10/QF-17). Sends use the UI path: ExecuteTaskCommand session.run when
// idle, queueMessage when busy. Approvals and questions are answered through
// session.respondApproval / respondInput exactly like the UI (QF-12/QF-13).
// `snapshot`, `verify-restart` and `capture` never send. --provider
// claude-code|codex runs the same steps for PD-14 comparison diffs.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {parseArgs} from 'node:util';
import {
  Evidence, Rpc, assertQaIdentity, assertRehearsalIdentity, captureWindow, captureWireSlice, finishEvidence, lunaSource,
  makeFixture, now, root, sliceFile, spawnFakeEngine, wireOffsets,
} from './qa-opencode-lib.mjs';
import {Driver} from './qa-opencode-driver.mjs';

const invoked = process.argv[2];
// `rehearse` runs the `run` path against a fake engine this process spawns.
const mode = invoked === 'rehearse' ? 'run' : invoked;
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
    answer: {type: 'string', default: 'none'},
    pick: {type: 'string'},
    capture: {type: 'boolean', default: false},
    ax: {type: 'boolean', default: false},
    'mock-url': {type: 'string'},
    'mock-log': {type: 'string'},
    'evidence-root': {type: 'string', default: path.join(root, 'target/qa/opencode')},
    run: {type: 'string'},
    action: {type: 'string'},
    label: {type: 'string'},
  },
});
if (!['run', 'snapshot', 'verify-restart', 'answer', 'capture'].includes(mode)) {
  console.error('usage: qa-opencode-live.mjs run|snapshot|verify-restart|answer|capture|rehearse [options] (see header)');
  process.exit(64);
}
const provider = opt.provider;
assert.ok(['opencode', 'claude-code', 'codex'].includes(provider), `unsupported provider ${provider}`);
assert.ok(['none', 'allow-once', 'deny', 'cancel'].includes(opt.answer), '--answer is none, allow-once, deny or cancel');
const isOpencode = provider === 'opencode';
const fake = invoked === 'rehearse' ? await spawnFakeEngine() : null;
const identity = fake ? assertRehearsalIdentity(fake.child, fake.port) : assertQaIdentity({provider});
const rehearsal = Boolean(fake);
const captureWire = isOpencode && !rehearsal;
const scenarioRoot = path.join(path.resolve(opt['evidence-root']), opt.scenario);

// An answer policy for waitTurn: approvals by --answer, questions by --pick.
const policy = pending => {
  if (pending.questions.some(q => q.approval)) return opt.answer === 'none' ? null : {approval: opt.answer};
  return opt.pick != null ? {pick: opt.pick} : null;
};

function parseAction(action) {
  assert.ok(action, '--action is required');
  if (['allow-once', 'allow-task', 'deny', 'cancel'].includes(action)) return {approval: action};
  if (action === 'dismiss') return {dismiss: true};
  const [kind, ...rest] = action.split(':');
  const value = rest.join(':');
  if (kind === 'pick') return {pick: value};
  if (kind === 'text') return {text: value};
  if (kind === 'mcp') return {mcp: value};
  throw new Error(`unknown --action ${action}`);
}

function loadRun() {
  const dir = path.resolve(opt.run ?? fs.readFileSync(path.join(scenarioRoot, 'latest'), 'utf8').trim());
  assert.ok(fs.existsSync(path.join(dir, 'manifest.json')), `no manifest in ${dir}`);
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
  const chatId = manifest.tasks?.root ?? manifest.tasks?.[manifest.requested?.provider];
  assert.ok(chatId, 'manifest has no chat to read');
  return {dir, manifest, chatId};
}

if (mode === 'capture') {
  let dir = opt.run ? path.resolve(opt.run) : null;
  if (!dir) {
    fs.mkdirSync(path.join(scenarioRoot, 'captures'), {recursive: true});
    dir = fs.mkdtempSync(path.join(scenarioRoot, 'captures', 'capture-'));
  }
  const evidence = new Evidence(dir, null);
  const record = captureWindow(evidence, identity.pid, opt.label ?? `capture-${Date.now()}`, {ax: opt.ax});
  console.log(JSON.stringify({dir, screenshot: record.screenshot, ax: record.ax}));
  process.exit(record.screenshot?.status === 'captured' || record.ax?.status === 'captured' ? 0 : 1);
}

let evidence;
let rehearsalFixture = null;
let rpc;
try {
  if (mode === 'run') {
    if (rehearsal) opt.tier = 'R0';
    else assert.ok(['T2', 'T3'].includes(opt.tier), '--tier T2 (mock provider) or T3 (real model) is required (QF-05)');
    const prompts = [...(opt.prompt ?? []), ...(opt['prompt-file'] ?? []).map(file => fs.readFileSync(file, 'utf8'))];
    if (prompts.length === 0) prompts.push(opt.tier === 'T2' ? '[[mock:text]] OpenCode QA live run: reply with the scripted text.' : 'Reply with exactly the word READY and nothing else. Do not use tools.');
    fs.mkdirSync(scenarioRoot, {recursive: true});
    const dir = fs.mkdtempSync(path.join(scenarioRoot, `run-${new Date().toISOString().replace(/[-:.]/g, '').slice(0, 15)}Z-`));
    fs.writeFileSync(path.join(scenarioRoot, 'latest'), `${dir}\n`);
    console.error(`evidence: ${dir}`);
    const fixture = makeFixture('luna-qa-opencode-live-');
    if (rehearsal) rehearsalFixture = path.dirname(fixture.dir);
    const manifest = {
      schema: 'luna-opencode-qa-manifest/1', mode, scenario: opt.scenario, tier: opt.tier, startedAt: now(), luna: lunaSource(),
      rehearsal,
      app: {name: identity.appName, bundleId: identity.bundleId, port: identity.port, pid: identity.pid, executable: identity.executable, dataDir: identity.dataDir, codesign: rehearsal ? 'n/a (rehearsal)' : 'verified (--deep --strict)', mcpIsolation: identity.lsenv.LUNA_QA_MCP_ISOLATION ?? null},
      requested: {provider, model: opt.model ?? null, variant: opt.variant ?? null, permissionMode: opt['permission-mode'], answer: opt.answer, pick: opt.pick ?? null},
      fixture: fixture.dir,
      xdg: identity.profile ? {isolated: true, profile: identity.profile.dir, homeIsolated: identity.profile.homeIsolated, offline: identity.profile.offline, projectConfigDisabled: identity.profile.projectConfigDisabled} : null,
      server: captureWire ? {ownership: 'spawned by the LUNA adapter', credentials: 'LUNA-private; never captured'} : null,
      wireLog: captureWire ? {...wireOffsets(identity.wireDir), start: null} : null,
      sends: [], tasks: {},
      replay: ['node', 'scripts/qa-opencode-live.mjs', ...process.argv.slice(2)].join(' '),
    };
    evidence = new Evidence(dir, manifest);
    evidence.phase = 'run';
    if (captureWire) {
      const out = evidence.file('binary-resolution.json');
      const argv = [path.join(root, 'scripts/qa-opencode-manifest.py'), 'binary', '--path', identity.profile.wrapper, '--out', out];
      const record = spawnSync('python3', argv, {encoding: 'utf8'});
      evidence.command({method: 'binary-record', params: {argv: ['python3', ...argv]}, outcome: record.status === 0 ? 'ok' : 'err'});
      manifest.opencodeBinary = fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, 'utf8')) : {error: record.stderr};
      manifest.wireLog.start = manifest.wireLog.files;
    }
    let mockBefore = null;
    if (opt['mock-url']) mockBefore = await (await fetch(`${opt['mock-url']}/__mock/state`)).json();
    const mockLogStart = opt['mock-log'] && fs.existsSync(opt['mock-log']) ? fs.statSync(opt['mock-log']).size : 0;
    evidence.writeManifest();

    rpc = await new Rpc({port: identity.port, log: record => evidence.command(record)}).connect();
    const driver = new Driver({rpc, evidence, identity, provider, nonce: null, ax: opt.ax});
    evidence.save('engine-info.json', await driver.init());
    const catalog = await rpc.call('ListModels', {harness: provider});
    evidence.save('catalog.json', catalog);
    const choose = () => {
      if (opt.model) return catalog.find(m => m.id === opt.model);
      if (isOpencode && opt.tier === 'T2') return catalog.find(m => m.id.startsWith('luna-mock/'));
      if (isOpencode) return catalog.find(m => /^opencode\/.*(-free|big-pickle)$/.test(m.id)) ?? catalog.find(m => m.isDefault);
      return catalog.find(m => m.isDefault) ?? catalog[0];
    };
    const model = choose();
    evidence.check('model-in-catalog', 'requested or tier default model listed by ListModels', model?.id ?? null, Boolean(model));
    assert.ok(model, 'no usable model in the LUNA catalog');
    const mockModel = model.id.startsWith('luna-mock/');
    if (!rehearsal) evidence.check('tier-label', opt.tier === 'T2' ? 'T2 uses the luna-mock provider' : 'T3 uses a real (non-mock) model', model.id, opt.tier === 'T2' ? mockModel : !mockModel);
    manifest.requested.model = model.id;
    const permissionMode = opt['permission-mode'];
    const config = {harness: provider, model: model.id, reasoning: opt.variant ?? null, modelOptions: {}, permissionMode, effectivePermissionMode: permissionMode,
      sandbox: permissionMode === 'full' ? 'danger-full-access' : 'workspace-write'};
    manifest.config = config;
    const spaceId = await driver.createProject(fixture.dir);
    const chatId = await driver.createChat({role: 'root', spaceId, cwd: fixture.dir, config, title: `OpenCode QA ${opt.scenario}`});
    evidence.check('chat-created', 'createChat applied', 'applied', true);
    await driver.snapshot('before');

    const turnTimeout = Number(opt['turn-timeout']) * 1000;
    for (const [index, text] of prompts.entries()) {
      await driver.runStep(`turn-${index}`, async () => {
        const sent = await driver.send(chatId, text, {label: `prompt ${index}`});
        const wait = await driver.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, answer: policy, timeoutMs: turnTimeout});
        const expected = opt['expect-status'];
        driver.check(`turn-${index}-settled`, `session reached ${expected} within the turn timeout`, `${wait.result}:${wait.status} via ${wait.statuses.join('>') || '-'}`, wait.result === 'settled' && wait.status === expected);
        if (wait.result === 'pending') driver.check(`turn-${index}-unanswered`, 'every approval/question had an answer policy (--answer/--pick)', wait.pending.questions.map(q => q.approval?.actionKind ?? 'question'), false);
        if (wait.handled.length) driver.info(`turn-${index}-answers`, 'answers sent through the UI path', wait.handled, wait.handled.every(h => h.status === 'applied'));
        if (opt.capture) driver.capture(`turn-${index}`);
        await driver.snapshot(`after-turn-${index}`);
      });
    }
    const after = await driver.snapshot('after');
    for (const send of manifest.sends.filter(s => s.kind === 'message')) {
      const count = after.chats.root.transcript.filter(entry => entry.id === send.messageId).length;
      evidence.check(`send-${send.index}-once`, 'the user message appears exactly once in the transcript', count, count === 1);
    }
    const record = await driver.chatRecord(chatId).catch(() => null);
    manifest.effective = {config: record?.config ?? null, title: record?.title ?? null};
    manifest.native = {sessionId: after.chats.root.identity?.currentBinding?.providerTaskId ?? null, readback: isOpencode ? 'wire log only (LUNA-private server)' : 'n/a'};
    driver.close();
    rpc = null;

    if (captureWire) {
      const slice = captureWireSlice(evidence, identity.wireDir, manifest.wireLog.start);
      manifest.wireLog = {...manifest.wireLog, end: slice.end, slices: slice.slices, frames: slice.frames, http: slice.http};
      evidence.check('wire-log-captured', 'LUNA_OPENCODE_WIRE_LOG present with bytes for this run (QF-11)', `${identity.wireDir ?? 'unset'}; ${slice.bytes} bytes`, slice.present && slice.bytes > 0);
    }
    if (opt['mock-url']) {
      const mockAfter = await (await fetch(`${opt['mock-url']}/__mock/state`)).json();
      manifest.mock = {url: opt['mock-url'], before: mockBefore, after: mockAfter};
      const requests = Object.values(mockAfter.counts ?? {}).reduce((a, b) => a + b, 0) - Object.values(mockBefore?.counts ?? {}).reduce((a, b) => a + b, 0);
      evidence.check('mock-requests', opt.tier === 'T2' ? 'the mock served this run' : 'the mock was not used by a T3 run', requests, opt.tier === 'T2' ? requests > 0 : requests === 0);
    } else if (opt.tier === 'T2') {
      evidence.check('mock-requests', 'T2 needs --mock-url to prove the mock served the run', 'no --mock-url', false);
    }
    if (opt['mock-log'] && fs.existsSync(opt['mock-log'])) manifest.mockLog = sliceFile(opt['mock-log'], mockLogStart, evidence.file('mock-requests.jsonl'));
    manifest.completed = true;
    manifest.modelUsage = {provider, model: model.id, tier: opt.tier, providerTurnsRequested: prompts.length};
    const passed = finishEvidence(evidence, {
      title: `OpenCode QA live run: ${manifest.scenario} (${invoked})`, tier: opt.tier, rehearsal,
      unverified: ['Native server readback (session record, message pages, pending asks) beyond what the wire log captured: the server password stays private to LUNA.'],
    });
    console.log(JSON.stringify({passed, evidence: dir, chatId}));
    process.exitCode = passed ? 0 : 1;
  } else {
    const {dir, manifest, chatId} = loadRun();
    evidence = new Evidence(dir, manifest);
    evidence.phase = mode;
    rpc = await new Rpc({port: identity.port, log: record => evidence.command({...record, mode})}).connect();
    const driver = new Driver({rpc, evidence, identity, provider: manifest.requested?.provider ?? provider, nonce: manifest.nonce ?? null});
    await driver.init();
    driver.attach(chatId, {role: 'root', config: manifest.config ?? {harness: manifest.requested?.provider}, cwd: manifest.fixture, spaceId: manifest.projectId});
    await driver.chats.get(chatId).mirror.ready;
    if (mode === 'snapshot') {
      const state = await driver.snapshot(`snapshot-${Date.now()}`);
      console.log(JSON.stringify({evidence: dir, pid: identity.pid, entries: state.chats.root.transcript.length}));
    } else if (mode === 'answer') {
      const mirror = driver.chats.get(chatId).mirror;
      const pending = mirror.currentPending();
      assert.ok(pending, 'no unresolved approval or question on this run\'s chat (QF-14)');
      const {receipt} = await driver.answer(chatId, pending, parseAction(opt.action), {label: 'operator answer'});
      console.log(JSON.stringify({evidence: dir, requestId: pending.requestId, status: receipt?.status ?? null, reason: receipt?.reason ?? null}));
      process.exitCode = receipt?.status === 'applied' ? 0 : 1;
    } else {
      assert.equal(manifest.completed, true, 'verify-restart needs a completed run');
      const restart = {mode, at: now(), pid: identity.pid, previousPid: manifest.app?.pid, criteria: []};
      const local = [];
      const record = (criterion, expected, actual, passed) => { local.push({criterion, expected, actual, passed: Boolean(passed)}); console.error(`[${passed ? 'PASS' : 'FAIL'}] ${criterion}: ${JSON.stringify(actual)}`); };
      record('app-restarted', 'a different app pid than the run', `${manifest.app?.pid} -> ${identity.pid}`, identity.pid !== manifest.app?.pid);
      const transcript = driver.chats.get(chatId).mirror.entries;
      for (const send of (manifest.sends ?? []).filter(s => s.messageId)) {
        const count = transcript.filter(entry => entry.id === send.messageId).length;
        record(`restart-send-${send.index}-once`, 'the user message survives restart exactly once', count, count === 1);
      }
      const queue = JSON.stringify(driver.chats.get(chatId).queue.latest ?? {});
      const replayed = (manifest.sends ?? []).filter(s => s.messageId && queue.includes(s.messageId)).length;
      record('restart-no-replayed-queue', 'no queue row for a recorded send after restart', replayed, replayed === 0);
      restart.criteria = local;
      restart.state = await driver.snapshot(`restart-${Date.now()}`);
      evidence.save(`restart-${Date.now()}.json`, restart);
      manifest.restarts = [...(manifest.restarts ?? []), {at: restart.at, pid: identity.pid, passed: local.every(c => c.passed)}];
      evidence.writeManifest();
      console.log(JSON.stringify({passed: local.every(c => c.passed), evidence: dir, restarted: true, pid: identity.pid}));
      process.exitCode = local.every(c => c.passed) ? 0 : 1;
    }
    driver.close();
    rpc = null;
    spawnSync('python3', [path.join(root, 'scripts/qa-opencode-manifest.py'), 'sums', '--dir', dir], {encoding: 'utf8'});
  }
} catch (error) {
  if (evidence) evidence.save(`failure-${mode}-${Date.now()}.json`, {message: String(error), stack: error?.stack, unknown: Boolean(error?.unknown), time: now()});
  throw error;
} finally {
  rpc?.close();
  fake?.child.kill('SIGTERM');
  // A rehearsal's fixture is disposable; real runs keep theirs for review.
  if (rehearsalFixture) fs.rmSync(rehearsalFixture, {recursive: true, force: true});
}
