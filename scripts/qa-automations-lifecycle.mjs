#!/usr/bin/env node
// SW-13 / E16: real local recurrence and host lifecycle on an isolated, signed
// QA app, driven over owner IPC (WebSocket JSON-RPC). Contract, phases and
// commands: docs/pending-tasks/luna-only-workflows/sw-13/README.md.
// Safety: all four LUNA_DEV_* overrides are required; the primary LUNA Dev is
// never addressed; approvals are never answered; quit and relaunch target only
// the verified QA executable and are never forced. Every mutation attempt is
// saved before it is sent. Uncertain work is never replayed. The duplicate
// oracle is a disposable sentinel file counted here, never an agent's claim.
// Exit: 0 every hard check passed, 1 a check failed, 2 a refused precondition.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync, spawnSync} from 'node:child_process';

class Refusal extends Error {}
class CheckFailure extends Error {}
const refuse = message => { throw new Refusal(message); };
const root = fs.realpathSync(path.resolve(import.meta.dirname, '..'));
const target = path.join(root, 'target');
const MAIN_PORT = 59777;
const SAFE_PATH = /^[A-Za-z0-9._/-]+$/;
const PHASES = ['runs', 'lifecycle', 'restart', 'failure'];
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const hex = bytes => crypto.randomBytes(bytes).toString('hex');

// ---------------------------------------------------------------- preflight
function preflight() {
  const keys = ['LUNA_DEV_APP_NAME', 'LUNA_DEV_BUNDLE_ID', 'LUNA_DEV_IPC_PORT', 'LUNA_DEV_DATA_DIR'];
  const missing = keys.filter(key => !process.env[key]);
  if (missing.length) refuse(`All four QA identity overrides are required; missing ${missing.join(', ')}`);
  for (const key of keys) if (!/^\S(?:[^\r\n]*\S)?$/.test(process.env[key])) refuse(`${key} must be nonempty and single-line`);
  const appName = process.env.LUNA_DEV_APP_NAME, bundleId = process.env.LUNA_DEV_BUNDLE_ID;
  if (!/^LUNA QA [A-Za-z0-9][A-Za-z0-9 -]{0,48}$/.test(appName) || appName.endsWith(' ')) refuse('LUNA_DEV_APP_NAME must be "LUNA QA <label>"');
  if (!/^com\.novnski\.luna\.qa\.[a-z][a-z0-9-]{0,47}$/.test(bundleId)) refuse('LUNA_DEV_BUNDLE_ID must be com.novnski.luna.qa.<slug>');
  if (!/^[1-9][0-9]{0,4}$/.test(process.env.LUNA_DEV_IPC_PORT)) refuse('LUNA_DEV_IPC_PORT must be a port number');
  const port = Number(process.env.LUNA_DEV_IPC_PORT);
  if (port < 1024 || port > 65535 || port === MAIN_PORT) refuse('A separate QA IPC port is required (not 59777)');
  if (fs.realpathSync(target) !== target) refuse('target/ must not redirect through symlinks');
  const dataDir = process.env.LUNA_DEV_DATA_DIR;
  if (!path.isAbsolute(dataDir) || !fs.existsSync(dataDir) || fs.realpathSync(dataDir) !== dataDir) refuse('LUNA_DEV_DATA_DIR must be an existing canonical absolute path (launch the QA app first)');
  const staging = path.join(target, 'luna-macos-dev');
  if (!dataDir.startsWith(`${target}${path.sep}`) || dataDir === staging || dataDir.startsWith(`${staging}${path.sep}`)) refuse('QA data must be inside this checkout target/ and outside bundle staging');
  const bundle = path.join(staging, `${appName}.app`);
  if (!fs.existsSync(bundle) || fs.realpathSync(bundle) !== bundle) refuse(`QA bundle is missing or redirected: ${bundle}`);
  const executable = path.join(bundle, 'Contents/MacOS/luna');
  const signed = spawnSync('codesign', ['--verify', '--deep', '--strict', bundle], {encoding: 'utf8'});
  if (signed.status !== 0) refuse(`codesign verification failed: ${signed.stderr.trim()}`);
  const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'}));
  const launchEnv = plist.LSEnvironment ?? {};
  if (plist.CFBundleIdentifier !== bundleId || plist.CFBundleDisplayName !== appName) refuse('Signed plist identity does not match the overrides');
  if (String(launchEnv.LUNA_IPC_PORT) !== String(port) || launchEnv.LUNA_DATA_DIR !== dataDir) refuse('Signed plist port or data directory does not match the overrides');
  if (String(launchEnv.LUNA_QA_MCP_ISOLATION) !== '1') refuse('The QA bundle must carry LUNA_QA_MCP_ISOLATION=1');
  for (const [key, value] of Object.entries(launchEnv)) if (!/^[A-Z][A-Z0-9_]*$/.test(key) || /[\r\n]/.test(String(value))) refuse(`Unsafe LSEnvironment entry ${key}`);
  const intervalMinutes = Number(process.env.LUNA_QA_SW13_INTERVAL_MINUTES ?? '2');
  if (!Number.isInteger(intervalMinutes) || intervalMinutes < 1 || intervalMinutes > 5) refuse('LUNA_QA_SW13_INTERVAL_MINUTES must be an integer from 1 to 5');
  const deadlineMinutes = Number(process.env.LUNA_QA_SW13_DEADLINE_MINUTES ?? '75');
  if (!Number.isInteger(deadlineMinutes) || deadlineMinutes < 10 || deadlineMinutes > 240) refuse('LUNA_QA_SW13_DEADLINE_MINUTES must be an integer from 10 to 240');
  let phases = PHASES;
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i++) {
    const value = argv[i] === '--phases' ? argv[++i] : argv[i].startsWith('--phases=') ? argv[i].slice(9) : refuse(`Unknown argument ${argv[i]}`);
    phases = String(value ?? '').split(',').filter(Boolean);
  }
  if (!phases.length || new Set(phases).size !== phases.length || phases.some(p => !PHASES.includes(p))) refuse(`--phases takes a comma list of ${PHASES.join(', ')}`);
  if (phases.includes('lifecycle') && !phases.includes('runs')) refuse('lifecycle reuses the runs automations; include runs');
  if (phases.includes('restart') && intervalMinutes < 2) refuse('restart timing needs LUNA_QA_SW13_INTERVAL_MINUTES of at least 2');
  const qaRoot = path.join(target, 'qa');
  const stamp = new Date().toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  const evidence = process.env.LUNA_QA_SW13_EVIDENCE_DIR ?? path.join(qaRoot, 'luna-only-workflows', `sw13-${stamp}`);
  if (!path.isAbsolute(evidence) || !evidence.startsWith(`${qaRoot}${path.sep}`) || !SAFE_PATH.test(evidence)) refuse('The evidence directory must be an absolute path inside target/qa without spaces or quotes');
  if (fs.existsSync(evidence) && fs.readdirSync(evidence).length) refuse('The evidence directory must be new or empty');
  return {appName, bundleId, port, dataDir, bundle, executable, plist, launchEnv, intervalMinutes, deadlineMinutes, phases: PHASES.filter(p => phases.includes(p)), evidence};
}
let cfg;
try { cfg = preflight(); } catch (error) {
  console.error(JSON.stringify({refused: String(error.message ?? error)}));
  process.exit(error instanceof Refusal ? 2 : 1);
}
const I = cfg.intervalMinutes * 60_000;
const startedAt = Date.now();
const deadline = startedAt + cfg.deadlineMinutes * 60_000;
const evidence = cfg.evidence;
fs.mkdirSync(evidence, {recursive: true});

// ---------------------------------------------------------------- evidence
let phase = 'preflight';
let waitingFor = null;
const checks = [];
const observations = [];
const elapsed = () => Math.round((Date.now() - startedAt) / 1000);
function progress(message, extra = {}) {
  console.log(JSON.stringify({at: new Date().toISOString(), elapsedS: elapsed(), phase, message, ...extra}));
}
const heartbeat = setInterval(() => progress('alive', {waitingFor}), 30_000);
heartbeat.unref();
const save = (name, value) => fs.writeFileSync(path.join(evidence, `${name}.json`), `${JSON.stringify(value, null, 2)}\n`);
const event = (name, value) => fs.appendFileSync(path.join(evidence, 'events.jsonl'), `${JSON.stringify({at: Date.now(), phase, name, value})}\n`);
// A hard check is recorded and stops the run on failure. A guard is the same
// check inside a polling loop, recorded only when it fails. An observation is
// evidence about provider or agent behaviour that never fails LUNA.
function check(id, name, ok, detail = null) {
  checks.push({id, phase, name, pass: Boolean(ok), detail});
  if (!ok) throw new CheckFailure(`${id} ${name}: ${JSON.stringify(detail)}`);
}
function guard(id, name, ok, detail = null) { if (!ok) check(id, name, ok, detail); }
function observe(id, name, detail) {
  observations.push({id, phase, name, detail});
  event('observation', {id, name, detail});
}

// ---------------------------------------------------------------- QA process
function listener(listenPort) {
  const result = spawnSync('lsof', ['-t', '-nP', `-iTCP:${listenPort}`, '-sTCP:LISTEN'], {encoding: 'utf8'});
  if (result.status === 1 && !result.stdout.trim()) return null;
  if (result.status !== 0) throw new Error(`Could not inspect IPC ${listenPort}`);
  const pids = [...new Set(result.stdout.trim().split(/\s+/))];
  if (pids.length !== 1 || !/^\d+$/.test(pids[0])) throw new Error(`Expected one listener on ${listenPort}, saw ${pids.join(',')}`);
  return Number(pids[0]);
}
function comm(pid) {
  const result = spawnSync('ps', ['-p', String(pid), '-o', 'comm='], {encoding: 'utf8'});
  return result.status === 0 ? result.stdout.trim() : null;
}
function lunaApps() {
  return execFileSync('ps', ['-axo', 'pid=,command='], {encoding: 'utf8'}).split('\n').flatMap(line => {
    const match = line.trim().match(/^(\d+)\s+(\/.*\.app\/Contents\/MacOS\/luna)(?:\s|$)/);
    return match ? [{pid: Number(match[1]), executable: match[2]}] : [];
  });
}
function singleQaApp(label, qaRunning) {
  if (listener(MAIN_PORT) !== null) refuse(`LUNA Dev (59777) must be closed during native QA (${label})`);
  const apps = lunaApps();
  const others = apps.filter(app => app.executable !== cfg.executable);
  if (others.length) refuse(`Another LUNA app is running (${label}): ${others.map(app => `${app.pid} ${app.executable}`).join('; ')}`);
  const own = apps.filter(app => app.executable === cfg.executable).length;
  if (own !== (qaRunning ? 1 : 0)) refuse(`Expected ${qaRunning ? 'one' : 'no'} QA process (${label}), saw ${own}`);
}
function verifiedListener() {
  const pid = listener(cfg.port);
  if (!pid) throw new Error('QA IPC is not listening');
  const executable = comm(pid);
  if (executable !== cfg.executable) throw new Error(`IPC ${cfg.port} belongs to ${executable}, not the QA executable`);
  return pid;
}

class Ipc {
  constructor(port) { this.port = port; this.seq = 0; this.pending = new Map(); this.closed = false; }
  async open() {
    const ws = new WebSocket(`ws://127.0.0.1:${this.port}`);
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { ws.close(); reject(new Error('QA IPC connect timeout')); }, 10_000);
      ws.addEventListener('open', () => { clearTimeout(timer); resolve(); }, {once: true});
      ws.addEventListener('error', () => { clearTimeout(timer); reject(new Error('QA IPC connect failed')); }, {once: true});
    });
    ws.addEventListener('message', message => {
      for (const line of String(message.data).split('\n').filter(Boolean)) {
        let frame;
        try { frame = JSON.parse(line); } catch { continue; }
        const waiter = this.pending.get(frame.id);
        if (!waiter || !['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) continue;
        this.pending.delete(frame.id);
        clearTimeout(waiter.timer);
        if (Object.hasOwn(frame, 'item')) this.send({id: frame.id, cancel: true});
        if (frame.err) waiter.reject(new Error(`${waiter.method}: ${JSON.stringify(frame.err)}`));
        else waiter.resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    });
    ws.addEventListener('close', () => {
      this.closed = true;
      for (const waiter of this.pending.values()) { clearTimeout(waiter.timer); waiter.reject(new Error(`${waiter.method}: IPC closed`)); }
      this.pending.clear();
    });
    this.ws = ws;
  }
  send(frame) { if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(frame)); }
  call(method, params = {}, ms = 45_000) {
    if (!this.ws || this.closed) return Promise.reject(new Error(`${method}: QA IPC is not connected`));
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        this.send({id, cancel: true});
        reject(new Error(`${method} timed out after ${ms / 1000} s; no automatic replay`));
      }, ms);
      this.pending.set(id, {resolve, reject, timer, method});
      this.ws.send(JSON.stringify({id, method, params}));
    });
  }
  close() { this.closed = true; try { this.ws?.close(); } catch {} }
}

let ipc;
let host;
async function connect(expectedPid) {
  ipc = new Ipc(cfg.port);
  await ipc.open();
  if (verifiedListener() !== expectedPid) throw new Error('QA listener changed while connecting');
  const info = await ipc.call('EngineInfo');
  if (info.workspaceScope !== 'local') refuse(`EngineInfo.workspaceScope is ${info.workspaceScope}, expected local`);
  const expected = sha256(Buffer.concat([Buffer.from('LUNA local root v1\0'), Buffer.from(cfg.dataDir)]));
  if ((await ipc.call('LocalOwnerRootClaim')).claim !== expected) refuse('LocalOwnerRootClaim does not prove this QA data directory');
  if (verifiedListener() !== expectedPid) throw new Error('QA listener changed during the ownership check');
  if (host !== undefined && info.deviceId !== host) refuse('The relaunched engine has another device identity');
  return info;
}

// ---------------------------------------------------------------- readers
async function until(label, ms, fn, everyMs = 1000) {
  const end = Math.min(Date.now() + ms, deadline);
  waitingFor = {label, boundS: Math.round((end - Date.now()) / 1000)};
  for (;;) {
    const value = await fn();
    if (value) { waitingFor = null; return value; }
    if (Date.now() >= end) throw new CheckFailure(`Timed out (${end === deadline ? 'run deadline' : `${Math.round(ms / 1000)} s`}): ${label}`);
    await delay(everyMs);
  }
}
async function sleepUntil(at, label) {
  if (at > deadline) throw new CheckFailure(`${label} is planned after the run deadline`);
  waitingFor = {label, untilIso: new Date(at).toISOString()};
  while (Date.now() < at) await delay(Math.min(5000, at - Date.now()));
  waitingFor = null;
}
const chats = async () => ipc.call('WatchChats');
const runsOf = async automationId => (await ipc.call('ListAutomationRuns', {automationId, limit: 100})).items;
const recordOf = async automationId => (await ipc.call('ListAutomations', {automationId, includeDeleted: true})).items[0];
const receiptOf = commandId => ipc.call('GetTaskCommandReceipt', {commandId});
const attention = () => ipc.call('GetAttention', {targetDeviceId: host});
async function transcript(taskId) {
  const value = await ipc.call('WatchDocMessages', {chatId: taskId});
  return value.frame?.reset ?? value.reset ?? [];
}
const textOf = entry => (entry?.parts ?? []).filter(p => p.kind === 'text').map(p => p.text).join('\n').trim();
const terminal = state => ['completed', 'failed', 'unknown', 'skipped', 'cancelled', 'resolved'].includes(state);
const admitted = state => ['dispatching', 'accepted'].includes(state);
const active = run => run.state === 'claimed' || admitted(run.state);
const attentionFor = (snapshot, automationId) => [...(snapshot.needsAttention ?? []), ...(snapshot.recent ?? [])]
  .filter(item => String(item.dedupKey ?? '').startsWith(`automation:${host}:${automationId}:`));
const within = (line, from, to) => line.epoch >= Math.floor(from / 1000) - 2 && line.epoch <= Math.ceil(to / 1000) + 2;
function sentinelLines(c) {
  if (!fs.existsSync(c.sentinel)) return [];
  return fs.readFileSync(c.sentinel, 'utf8').split('\n').filter(Boolean).map(line => {
    const [marker, epoch] = line.trim().split(/\s+/);
    return {marker, epoch: Number(epoch), line};
  });
}

// ---------------------------------------------------------------- mutations
async function mutate(name, automationId, expectedRevision, operation) {
  const request = {requestId: crypto.randomUUID(), automationId, ownerHostId: host, expectedRevision, operation};
  for (let attempt = 1; ; attempt++) {
    const preview = await ipc.call('PreviewAutomation', request);
    request.previewId = preview.previewId;
    event('mutation-attempt', {name, attempt, request, preview});
    try {
      const receipt = await ipc.call('MutateAutomation', request);
      event('mutation-receipt', {name, receipt});
      return receipt;
    } catch (error) {
      // A lost or refused response proves nothing about the commit. Read the
      // receipt by request id, then retry the identical request once.
      const saved = await ipc.call('GetAutomationMutationReceipt', {requestId: request.requestId});
      event('mutation-error', {name, attempt, error: String(error), saved});
      if (saved) return saved;
      if (attempt >= 2) throw error;
    }
  }
}
async function pause(c) {
  const record = await recordOf(c.automationId);
  if (!record.enabled || record.deleted) { c.pausedAtMs ??= record.updatedAtMs; return; }
  c.pausedAtMs = (await mutate(`${c.id}-pause`, c.automationId, record.revision, {kind: 'pause'})).committedAtMs;
}
async function accountChange(method, harness, accountId) {
  for (;;) {
    try {
      const snapshot = await ipc.call(method, {harness, accountId}, 90_000);
      const accounts = (snapshot.accounts ?? []).filter(a => a.harness === harness).map(a => ({id: a.id, active: a.active, authentication: a.authentication}));
      event('account-change', {method, harness, accountId, accounts});
      return accounts;
    } catch (error) {
      if (!/still running|did not retire/i.test(String(error)) || Date.now() > deadline) throw error;
      progress('provider still busy; retrying the account change', {method, harness});
      await delay(3000);
    }
  }
}

// ---------------------------------------------------------------- fixture
let project;
let spaceId;
const sources = {};
const models = {};
const cases = {};
const disconnected = new Map();
const created = new Set();
const preferred = {
  codex: ['gpt-6-luna', 'gpt-6.1-sol'],
  claude: ['claude-sonnet-5-5', 'claude-sonnet-4-6', 'claude-sonnet-4-5'],
  claudeAuto: ['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-sonnet-4-6'],
};
async function pickModel(provider, mode) {
  const key = `${provider}:${mode}`;
  if (models[key]) return models[key];
  // A freshly started engine may still be loading accounts or the registry;
  // only that explicitly retryable refusal is retried, for at most 30 s.
  let catalog;
  for (let attempt = 1, end = Math.min(Date.now() + 30_000, deadline); ; attempt++) {
    try { catalog = await ipc.call('ListModels', {harness: provider}, 90_000); break; } catch (error) {
      if (!/changed during model discovery; retry/i.test(String(error)) || Date.now() >= end) throw error;
      observe('F2', `${key} model discovery asked for a retry`, {attempt, error: String(error)});
      await delay(Math.min(1000 * attempt, 5000));
    }
  }
  const override = provider === 'codex' ? process.env.LUNA_QA_CODEX_MODEL
    : (mode === 'auto' && process.env.LUNA_QA_CLAUDE_AUTO_MODEL) || process.env.LUNA_QA_CLAUDE_MODEL;
  const list = provider === 'codex' ? preferred.codex : mode === 'auto' ? preferred.claudeAuto : preferred.claude;
  const model = override ? catalog.find(m => m.id === override)
    : list.map(id => catalog.find(m => m.id === id)).find(Boolean) ?? catalog.find(m => m.isDefault) ?? catalog[0];
  check('F2', `${key} model is in the live catalog`, model, {override, catalog: catalog.map(m => m.id)});
  const levels = model.reasoningLevels ?? [];
  models[key] = {id: model.id, reasoning: levels.includes('low') ? 'low' : levels[0]};
  return models[key];
}
async function sourceChat(provider, mode, label) {
  const key = [provider, mode, label].filter(Boolean).join('-');
  if (sources[key]) return sources[key];
  const model = await pickModel(provider, mode);
  const chatId = crypto.randomUUID();
  const config = {harness: provider, model: model.id, reasoning: model.reasoning, sandbox: 'workspace-write', permissionMode: mode, effectivePermissionMode: mode};
  event('create-chat', {chatId, config});
  await ipc.call('Mutate', {op: 'createChat', chatId, spaceId, cwd: project, config, title: `SW-13 ${key}`});
  // A schedule approved before checkout discovery is correctly refused later.
  const row = await until(`${key} checkout identity`, 30_000, async () => {
    const chat = (await chats()).find(c => c.id === chatId);
    return chat?.checkoutId && chat.cwd === project && chat;
  });
  check('F2', `${key} source chat keeps provider and mode`, row.config?.harness === provider && row.config?.permissionMode === mode, row.config);
  sources[key] = {key, chatId, provider, mode, model};
  save(`chat-${key}`, row);
  return sources[key];
}
// Claude Code's Bash tool refuses a shell `sleep` chained before a command
// (2.1.293), and the model then backgrounds it, so nothing stays in flight.
// A foreground interpreter wait is accepted by both CLIs.
const appendCommand = (c, marker) => `python3 -c 'import time; time.sleep(${Number(c.sleepS)})'; printf '%s %s\\n' '${marker}' "$(date +%s)" >> '${c.sentinel}'`;
function runPrompt(c, marker) {
  return [
    `SW-13 scheduled QA occurrence (${c.id}). Run exactly this one shell command once, in the foreground, and wait for it to finish (about ${Number(c.sleepS)} seconds):`,
    appendCommand(c, marker),
    'Do not run it in the background, split it, or shorten or remove the wait.',
    `Then reply with exactly ${marker} and nothing else.`,
    'Do not run any other command, read or change other files, contact any service or create other work.',
  ].join('\n');
}
const markerFor = (c, run) => c.markerByRevision[run.automationRevision] ?? c.marker;
async function newCase(id, provider, mode, destination, options) {
  const source = await sourceChat(provider, mode, options.chat);
  const c = {
    id, provider, mode, destination, sourceChatId: source.chatId, model: source.model, automationId: crypto.randomUUID(),
    marker: options.marker ?? `SW13_${id.replace(/[^a-z0-9]+/gi, '_').toUpperCase()}_${hex(4)}`,
    sentinel: path.join(project, 'sentinels', `${id}.log`), sentinelName: `sentinels/${id}.log`, fixture: options.fixture ?? null,
    sleepS: options.sleepS ?? 6, maxLatenessMs: options.maxLatenessMs ?? 600_000,
    notificationPolicy: options.notificationPolicy ?? (destination === 'existing' ? 'allRuns' : 'importantUpdates'),
    trigger: options.trigger, markers: [], markerByRevision: {}, runs: [], samples: [], verified: {},
  };
  c.execution = {harness: provider, model: c.model.id, reasoning: c.model.reasoning, modelOptions: {}};
  c.prompt = options.prompt ?? runPrompt(c, c.marker);
  c.markers.push(c.marker);
  const definition = {
    name: `SW-13 ${id}`, prompt: c.prompt, timezone: 'Europe/Bucharest',
    target: destination === 'existing' ? {kind: 'existing', taskId: c.sourceChatId} : {kind: 'newConversation', originTaskId: c.sourceChatId, projectId: spaceId},
    trigger: c.trigger, maxLatenessMs: c.maxLatenessMs, execution: c.execution, sourceTaskId: c.sourceChatId, notificationPolicy: c.notificationPolicy,
  };
  const receipt = await mutate(`${id}-save`, c.automationId, 0, {kind: 'save', definition});
  created.add(c.automationId);
  const saved = receipt.record;
  check('F2', `${id} saved execution, project and mode`, saved.definition.execution?.model === c.model.id && saved.authority.projectId === spaceId
    && saved.authority.permissionCeiling === mode && saved.authority.config?.permissionMode === mode, {execution: saved.definition.execution, authority: saved.authority});
  cases[id] = c;
  save(`case-${id}-saved`, {case: c, receipt});
  return c;
}
const interval = anchorMs => ({kind: 'interval', anchorMs, everyMinutes: cfg.intervalMinutes});

// Sample every listed case once: its runs (newest first) and, for admitted
// runs, the live execution summary of the target conversation.
async function sample(list) {
  const rows = await chats();
  for (const c of list) {
    c.runs = await runsOf(c.automationId);
    for (const run of c.runs.filter(r => admitted(r.state))) {
      const running = rows.find(chat => chat.id === run.taskId)?.execution?.running;
      if (running) c.samples.push({runId: run.id, at: Date.now(), running});
    }
    const states = c.runs.map(r => `${r.id.slice(-6)}:${r.state}`).join(',');
    if (states !== c.lastStates) { c.lastStates = states; progress('runs', {case: c.id, runs: states}); }
  }
  return rows;
}
async function waitFor(list, label, ms, done, everyMs = 1000) {
  return until(label, ms, async () => {
    const rows = await sample(list);
    return done(rows) && rows;
  }, everyMs);
}

// ---------------------------------------------------------------- per-run proof
async function verifyRun(c, run, marker = markerFor(c, run)) {
  const tag = `${c.id} ${run.id.slice(-8)}`;
  const replyOf = all => {
    const index = all.findIndex(e => e.id === run.messageId && e.role === 'user');
    if (index < 0) return null;
    const next = all.findIndex((e, i) => i > index && e.role === 'user');
    return {index, reply: all.slice(index + 1, next < 0 ? undefined : next).filter(e => e.role === 'assistant')};
  };
  const entries = await until(`${tag} transcript`, 45_000, async () => {
    const all = await transcript(run.taskId);
    return replyOf(all)?.reply.at(-1)?.status === 'complete' && all;
  });
  const {index, reply} = replyOf(entries);
  const parts = reply.flatMap(e => e.parts ?? []);
  const tools = parts.filter(p => p.kind === 'tool').map(p => ({id: p.id, call: p.call, isError: p.isError, resolved: p.resolved, output: p.output}));
  const inputs = parts.filter(p => p.kind === 'input').map(p => ({id: p.id, resolved: p.resolved}));
  const exec = tools.filter(t => t.call?.kind === 'exec' && String(t.call.command ?? '').includes(c.fixture ?? c.sentinelName));
  const finalText = reply.map(textOf).filter(Boolean).at(-1) ?? '';
  const send = await receiptOf(run.commandId);
  const create = c.destination === 'project' ? await receiptOf(`${run.commandId}:create`) : null;
  const chat = (await chats()).find(row => row.id === run.taskId);
  const proof = {
    runId: run.id, state: run.state, revision: run.automationRevision, scheduledAtMs: run.scheduledAtMs, claimedAtMs: run.claimedAtMs,
    updatedAtMs: run.updatedAtMs, coalesced: run.coalesced, taskId: run.taskId, messageId: run.messageId, commandId: run.commandId,
    detail: run.detail, userMessages: entries.filter(e => e.id === run.messageId).length, userText: textOf(entries[index]),
    tools, inputs, finalText, receipt: send && {status: send.status, correlationId: send.correlationId, reason: send.reason},
    createReceipt: create && {status: create.status}, chatConfig: chat?.config, samples: c.samples.filter(s => s.runId === run.id),
    execOk: exec.some(t => t.resolved && !t.isError), execCalls: exec.length,
  };
  c.verified[run.id] = proof;
  save(`run-${c.id}-${run.id.slice(-12)}`, {proof, transcript: entries});
  check('F1', `${tag} delivered its exact message once`, proof.userMessages === 1 && proof.userText.includes(marker), {userMessages: proof.userMessages, userText: proof.userText});
  check('F1', `${tag} reached its destination`, c.destination === 'existing' ? run.taskId === c.sourceChatId : run.taskId !== c.sourceChatId, {taskId: run.taskId, source: c.sourceChatId});
  check('F4', `${tag} Run receipt applied for this message`, send?.status === 'applied' && send.correlationId === run.messageId, proof.receipt);
  if (c.destination === 'project') check('F4', `${tag} fresh conversation creation applied`, create?.status === 'applied', proof.createReceipt);
  check('F2', `${tag} conversation runs the saved provider, model and mode`, chat?.config?.harness === c.provider && chat.config.model === c.model.id && chat.config.permissionMode === c.mode, chat?.config);
  check('F2', `${tag} no question was left for a person`, inputs.every(i => i.resolved), inputs);
  for (const s of proof.samples) {
    check('F2', `${tag} live execution matched while admitted`, s.running.provider === c.provider && s.running.model === c.model.id && s.running.permissionMode === c.mode, s.running);
  }
  check('F2', `${tag} used its shell tool`, exec.length >= 1, tools);
  if (c.fixture) return proof;
  if (c.mode === 'full') check('F2', `${tag} Full ran the tool without a decline`, proof.execOk, tools);
  else if (!proof.execOk) observe('F2', `${tag} Auto's provider reviewer declined or the tool failed`, tools);
  // One automation never has overlapping occurrences (P2), so the run's own
  // window holds exactly its write, or nothing when its tool did not succeed.
  const own = sentinelLines(c).filter(line => within(line, run.claimedAtMs, run.updatedAtMs));
  check('F5', `${tag} wrote the sentinel exactly as its tool reported`, own.length === (proof.execOk ? 1 : 0) && own.every(l => l.marker === marker), {own, execOk: proof.execOk});
  if (!finalText.includes(marker)) observe('F1', `${tag} reply text differs from the requested marker`, finalText.slice(0, 400));
  return proof;
}

// The independent duplicate oracle over a settled case: every sentinel line
// maps to exactly one delivered or uncertain run window; delivered runs wrote
// exactly as their tool reported; nothing else wrote the file.
function sentinelOracle(c, label) {
  const lines = sentinelLines(c);
  const windows = [
    ...Object.values(c.verified).map(p => ({runId: p.runId, from: p.claimedAtMs, to: p.updatedAtMs, execOk: p.execOk})),
    ...c.runs.filter(r => r.state === 'unknown' || r.state === 'resolved').map(r => ({runId: r.id, from: r.claimedAtMs, to: r.updatedAtMs, uncertain: true})),
  ];
  const mapped = lines.map(line => ({...line, runs: windows.filter(w => within(line, w.from, w.to)).map(w => w.runId)}));
  const perRun = Object.fromEntries(windows.map(w => [w.runId, mapped.filter(l => l.runs.includes(w.runId)).length]));
  const result = {lines: mapped, perRun, windows};
  save(`sentinel-${c.id}-${label}`, result);
  check('F5', `${c.id} every sentinel line belongs to exactly one run of this case`, mapped.every(l => l.runs.length === 1 && c.markers.includes(l.marker)), mapped);
  check('F5', `${c.id} no run wrote twice`, Object.values(perRun).every(n => n <= 1), perRun);
  check('F5', `${c.id} delivered runs wrote exactly as their tools reported`, windows.filter(w => !w.uncertain).every(w => perRun[w.runId] === (w.execOk ? 1 : 0)), windows);
  return result;
}
// Pause first; then nothing new is claimed and every completed run is proved.
async function settle(c, label) {
  await pause(c);
  await waitFor([c], `${c.id} has no claimed or admitted occurrence`, 300_000, () => !c.runs.some(active));
  for (const run of c.runs.filter(r => r.state === 'completed' && !c.verified[r.id])) await verifyRun(c, run);
  return sentinelOracle(c, label);
}
function attentionCheck(c, snapshot, kind, run) {
  const items = attentionFor(snapshot, c.automationId);
  const item = items.find(i => i.kind === kind && (!run || i.source?.messageId === run.messageId));
  const muted = kind === 'automationCompleted' ? c.notificationPolicy !== 'allRuns' : c.notificationPolicy === 'never';
  check('F3', `${c.id} Activity shows ${kind} under ${c.notificationPolicy}`, item && item.suppressNotification === muted, {items, muted, run: run?.id});
  return item;
}
// Activity projects the latest occurrence of a settled, nondeleted definition.
function settledAttention(c, snapshot) {
  const latest = c.runs[0];
  if (!latest || latest.state === 'resolved' || latest.state === 'cancelled') return;
  attentionCheck(c, snapshot, latest.state === 'completed' ? 'automationCompleted' : 'automationFailed', latest);
}

// ---------------------------------------------------------------- phases
async function setup() {
  phase = 'setup';
  project = path.join(evidence, 'project');
  fs.mkdirSync(path.join(project, 'sentinels'), {recursive: true});
  fs.mkdirSync(path.join(project, 'fixture'), {recursive: true});
  fs.writeFileSync(path.join(project, 'README.md'), 'Disposable SW-13 automation QA project. Scheduled turns append to sentinels/ only; never change other files or contact services.\n');
  fs.writeFileSync(path.join(project, 'fixture', 'tool-fail.sh'), [
    '#!/bin/sh', '# SW-13 disposable fixture: record the call, then fail like a broken tool.',
    'printf \'%s %s\\n\' "${1:-unknown}" "$(date +%s)" >> "$(dirname "$0")/tool-calls.log"',
    'echo "SW13 fixture tool failure: simulated error (exit 7)" >&2', 'exit 7', '',
  ].join('\n'), {mode: 0o755});
  execFileSync('git', ['init', '-q', '-b', 'main', project]);
  spaceId = crypto.randomUUID();
  await ipc.call('Mutate', {op: 'createSpace', spaceId, deviceId: host, path: project, gitDetected: true, name: 'SW-13 Recurrence QA'});
  event('create-space', {spaceId, project});
}

const RUN_CASES = ['codex', 'claude-code'].flatMap(provider => ['full', 'auto'].flatMap(mode => ['existing', 'project'].map(destination => ({provider, mode, destination, id: `${provider}-${mode}-${destination}`}))));
async function phaseRuns() {
  phase = 'runs';
  for (const {provider, mode} of RUN_CASES) await sourceChat(provider, mode);
  const anchor0 = Date.now() + 75_000;
  for (const [index, {provider, mode, destination, id}] of RUN_CASES.entries()) {
    await newCase(id, provider, mode, destination, {trigger: interval(anchor0 + index * 6_000)});
  }
  const list = RUN_CASES.map(({id}) => cases[id]);
  progress('scheduled', {cases: list.map(c => c.id), firstSlot: new Date(anchor0).toISOString(), intervalMinutes: cfg.intervalMinutes});
  await waitFor(list, 'two completed occurrences per destination', 75_000 + 7 * 6_000 + I + 300_000, () => {
    for (const c of list) {
      const bad = c.runs.find(r => r.state === 'failed' || r.state === 'unknown');
      guard('F1', `${c.id} occurrences stay deliverable`, !bad, bad);
    }
    return list.every(c => c.runs.filter(r => r.state === 'completed').length >= 2);
  });
  for (const c of list) {
    const completed = c.runs.filter(r => r.state === 'completed').sort((a, b) => a.scheduledAtMs - b.scheduledAtMs);
    const skipped = c.runs.filter(r => r.state === 'skipped');
    if (skipped.length) observe('P2', `${c.id} skipped slots while busy`, skipped.map(r => ({id: r.id, detail: r.detail})));
    for (const run of completed) {
      check('F1', `${c.id} ${run.id.slice(-8)} is an aligned slot of revision 1`, run.automationRevision === 1 && (run.scheduledAtMs - c.trigger.anchorMs) % I === 0 && run.coalesced === 0, run);
      await verifyRun(c, run);
    }
    check('F1', `${c.id} two distinct real occurrences`, completed[0].id !== completed[1].id && (c.destination === 'existing' || completed[0].taskId !== completed[1].taskId), completed.map(r => r.taskId));
    check('F2', `${c.id} execution was observed live at least once`, c.samples.length > 0, c.samples.length);
  }
  const contracts = {};
  for (const taskId of new Set(list.flatMap(c => Object.values(c.verified).map(p => p.taskId)))) {
    contracts[taskId] = await ipc.call('GetRuntimeContract', {taskId}).catch(error => ({error: String(error)}));
  }
  save('runs-readback', {cases: list, runtimeContracts: contracts});
  if (!cfg.phases.includes('lifecycle')) {
    for (const c of list) await settle(c, 'runs');
    const snapshot = await attention();
    for (const c of list) settledAttention(c, snapshot);
    save('runs-attention', snapshot);
  }
}

async function phaseLifecycle() {
  phase = 'lifecycle';
  const update = cases['codex-full-existing'], doomed = cases['claude-code-auto-project'];
  const paused = RUN_CASES.map(({id}) => cases[id]).filter(c => c !== update && c !== doomed);
  for (const c of paused) await pause(c);
  // Update: a new prompt and marker under the next revision.
  const before = await recordOf(update.automationId);
  const marker2 = `${update.marker}_V2`;
  const definition = {...before.definition, name: `${before.definition.name} v2`, prompt: runPrompt(update, marker2)};
  const updated = await mutate(`${update.id}-update`, update.automationId, before.revision, {kind: 'save', definition});
  update.updatedAtMs = updated.committedAtMs;
  update.markers.push(marker2);
  update.markerByRevision[updated.record.revision] = marker2;
  check('F6', `${update.id} update committed a new revision without changing execution`, updated.record.revision === before.revision + 1
    && updated.record.definition.execution?.model === update.model.id && updated.record.enabled, updated.record);
  // Delete while an occurrence is admitted: accepted by the provider, or its
  // Run command applied.
  let victim;
  await until(`${doomed.id} occurrence admitted`, 2 * I + 180_000, async () => {
    await sample([doomed, update]);
    const candidate = doomed.runs.find(r => admitted(r.state));
    if (!candidate) return false;
    if (candidate.state === 'accepted') return (victim = candidate);
    return (await receiptOf(candidate.commandId))?.status === 'applied' && (victim = candidate);
  }, 500);
  const live = await recordOf(doomed.automationId);
  const deleted = await mutate(`${doomed.id}-delete`, doomed.automationId, live.revision, {kind: 'delete'});
  doomed.deletedAtMs = deleted.committedAtMs;
  check('F7', `${doomed.id} delete committed a tombstone`, deleted.record.deleted && !deleted.record.enabled, deleted.record);
  await waitFor([doomed, update], `${update.id} new revision ran and ${doomed.id} admitted run ended`, I + 240_000, () =>
    terminal(doomed.runs.find(r => r.id === victim.id)?.state) && update.runs.some(r => r.automationRevision === updated.record.revision && r.state === 'completed'));
  const ended = doomed.runs.find(r => r.id === victim.id);
  check('F7', `${doomed.id} already admitted run finished in its conversation`, ended.state === 'completed', ended);
  await verifyRun(doomed, ended);
  // One more slot: nothing new for the deleted or paused definitions.
  await sleepUntil(Date.now() + I + 30_000, 'one more slot with deleted and paused definitions');
  await sample([...paused, doomed, update]);
  check('F7', `${doomed.id} no occurrence after delete`, doomed.runs.every(r => r.claimedAtMs < doomed.deletedAtMs), doomed.runs);
  for (const c of paused) {
    check('F8', `${c.id} no occurrence claimed after pause`, c.runs.every(r => r.claimedAtMs < c.pausedAtMs), c.runs);
    check('F15', `${c.id} pausing never left uncertain delivery`, !c.runs.some(r => r.state === 'unknown'), c.runs);
  }
  check('F6', `${update.id} no old-revision occurrence after the update`, update.runs.every(r => r.automationRevision >= updated.record.revision || r.claimedAtMs <= update.updatedAtMs), update.runs);
  const oracles = {};
  for (const c of [...paused, update, doomed]) oracles[c.id] = await settle(c, 'lifecycle');
  // The old prompt may still be written only by work claimed before the update.
  const oracle = oracles[update.id];
  const owner = line => update.runs.find(r => r.id === line.runs[0]);
  const lateOld = oracle.lines.filter(l => l.marker === update.marker && l.epoch * 1000 > update.updatedAtMs + 2000);
  check('F6', `${update.id} old prompt wrote after the update only from work claimed before it`, lateOld.every(l => owner(l)?.claimedAtMs <= update.updatedAtMs), lateOld);
  const newLines = oracle.lines.filter(l => l.marker === marker2);
  check('F6', `${update.id} new prompt was written only by the new revision`, newLines.length >= 1 && newLines.every(l => owner(l)?.automationRevision >= updated.record.revision), newLines);
  const snapshot = await attention();
  check('F7', `${doomed.id} deleted definition left Activity`, attentionFor(snapshot, doomed.automationId).length === 0, attentionFor(snapshot, doomed.automationId));
  for (const c of [...paused, update]) settledAttention(c, snapshot);
  save('lifecycle-readback', {update, doomed, paused, attention: snapshot});
}

// Graceful quit of the exact QA process under the shared app-operation lock.
let appLock = null;
function takeAppLock() {
  const common = execFileSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], {cwd: root, encoding: 'utf8'}).trim();
  const lock = path.join(common, 'luna-app-operation.lock');
  try { fs.mkdirSync(lock); } catch { refuse(`Another LUNA app operation owns ${lock}; inspect its pid before retrying`); }
  fs.writeFileSync(path.join(lock, 'pid'), String(process.pid));
  appLock = lock;
}
function releaseAppLock() {
  if (!appLock) return;
  try { fs.rmSync(path.join(appLock, 'pid'), {force: true}); fs.rmdirSync(appLock); } catch {}
  appLock = null;
}
process.once('exit', releaseAppLock);
async function quitQaApp() {
  const old = verifiedListener();
  ipc.close();
  const quitStart = Date.now();
  const asked = spawnSync('osascript', ['-l', 'JavaScript', '-e', `ObjC.import("AppKit"); function run(argv) {
    const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
    if (app.isNil() || ObjC.unwrap(app.executableURL.path) !== argv[1]) throw new Error("QA identity changed");
    app.terminate;
  }`, String(old), cfg.executable], {encoding: 'utf8'});
  event('quit-request', {pid: old, status: asked.status, stderr: String(asked.stderr ?? '').trim()});
  // AppKit may report -128 while LUNA saves and then quits itself; the
  // process exit is the authoritative result. Never force it.
  await until('QA app graceful quit', 60_000, async () => comm(old) !== cfg.executable, 250);
  check('F11', 'QA IPC port is free after the graceful quit', listener(cfg.port) === null, cfg.port);
  singleQaApp('after quit', false);
  const quit = {pid: old, quitStart, quitDone: Date.now()};
  event('quit', quit);
  progress('QA app quit gracefully', quit);
  return quit;
}
async function launchQaApp(oldPid) {
  singleQaApp('before relaunch', false);
  const envArgs = Object.entries(cfg.launchEnv).flatMap(([key, value]) => ['--env', `${key}=${value}`]);
  const launchStart = Date.now();
  execFileSync('open', ['-g', '-n', ...envArgs, cfg.bundle], {stdio: 'pipe'});
  const fresh = await until('QA app relaunch listener', 90_000, async () => {
    const current = listener(cfg.port);
    return current && comm(current) === cfg.executable && current;
  }, 500);
  check('F11', 'relaunch started a fresh QA process', fresh !== oldPid, {fresh, oldPid});
  singleQaApp('after relaunch', true);
  await until('relaunched owner IPC', 60_000, async () => {
    try { await connect(fresh); return true; } catch (error) { if (error instanceof Refusal) throw error; ipc?.close(); return false; }
  });
  const launch = {pid: fresh, launchStart, listening: Date.now()};
  event('launch', launch);
  progress('QA app relaunched', launch);
  return launch;
}
// Documented policy (P3) applied to the observed first claim after relaunch:
// the same fold and tolerance the sweep computes at that instant.
function expectedAfterRestart(c, nextDueMs, run) {
  const folded = Math.floor((run.claimedAtMs - nextDueMs) / I);
  const slot = nextDueMs + folded * I;
  return {slot, folded, late: run.claimedAtMs - slot > c.maxLatenessMs};
}

async function phaseRestart() {
  phase = 'restart';
  const base = Date.now() + 45_000;
  const coalesce = await newCase('restart-coalesce', 'codex', 'full', 'project', {trigger: interval(base), notificationPolicy: 'importantUpdates'});
  const skip = await newCase('restart-skip', 'claude-code', 'full', 'existing', {trigger: interval(base + 6_000), maxLatenessMs: 20_000, notificationPolicy: 'allRuns'});
  const inflight = await newCase('restart-inflight', 'claude-code', 'full', 'existing', {trigger: interval(base + 70_000), sleepS: 45, chat: 'inflight', notificationPolicy: 'allRuns'});
  const subjects = [coalesce, skip, inflight];
  await waitFor(subjects, 'restart subjects ran their first slot', 6_000 + 240_000, () => {
    for (const c of [coalesce, skip]) guard('F9', `${c.id} first slot stays deliverable`, !c.runs.some(r => ['failed', 'unknown', 'skipped'].includes(r.state)), c.runs);
    return [coalesce, skip].every(c => c.runs.some(r => r.state === 'completed'));
  });
  for (const c of [coalesce, skip]) await verifyRun(c, c.runs.find(r => r.state === 'completed'));
  let held;
  await waitFor(subjects, `${inflight.id} admitted with its tool running`, 70_000 + 240_000, () => (held = inflight.runs.find(r => r.state === 'accepted')), 500);
  await delay(8_000);
  await sample(subjects);
  held = inflight.runs.find(r => r.id === held.id);
  check('F11', `${inflight.id} is still admitted at quit`, held.state === 'accepted' && sentinelLines(inflight).length === 0, {held, lines: sentinelLines(inflight)});
  check('F11', 'no other restart subject is active at quit', [coalesce, skip].every(c => !c.runs.some(active)), [coalesce.runs, skip.runs]);
  const due = {};
  for (const c of subjects) due[c.id] = (await recordOf(c.automationId)).nextDueMs;
  save('restart-before-quit', {due, runs: Object.fromEntries(subjects.map(c => [c.id, c.runs])), lines: Object.fromEntries(subjects.map(c => [c.id, sentinelLines(c)])), transcript: await transcript(inflight.sourceChatId)});
  takeAppLock();
  const quit = await quitQaApp();
  // Stay down across at least two slots of both timed subjects, and relaunch
  // well past a restart-skip slot (beyond its 20 s tolerance, before the next).
  const offset = Math.min(45_000, I / 2);
  let k = 1;
  let relaunchAt = due[skip.id] + k * I + offset;
  while (relaunchAt - due[coalesce.id] < I + 10_000 || relaunchAt < quit.quitDone + 30_000) relaunchAt = due[skip.id] + ++k * I + offset;
  progress('host down across due slots', {relaunchAt: new Date(relaunchAt).toISOString(), due});
  await sleepUntil(relaunchAt, 'relaunch time (host down across due slots)');
  const launch = await launchQaApp(quit.pid);
  releaseAppLock();
  event('downtime', {quit, launch, due});
  await waitFor(subjects, 'first post-restart claims', 120_000, () => [coalesce, skip].every(c => c.runs.some(r => r.claimedAtMs >= launch.launchStart)));
  const outcomes = {};
  for (const c of [coalesce, skip]) {
    const after = c.runs.filter(r => r.claimedAtMs >= launch.launchStart).sort((a, b) => a.claimedAtMs - b.claimedAtMs);
    const first = after[0];
    const expected = expectedAfterRestart(c, due[c.id], first);
    outcomes[c.id] = {first, expected};
    check('F9', `${c.id} no catch-up burst after restart`, after.filter(r => r.claimedAtMs < first.claimedAtMs + 30_000).length === 1, after);
    check('F14', `${c.id} nothing was claimed while the host was down`, c.runs.every(r => r.claimedAtMs < quit.quitStart || r.claimedAtMs >= launch.launchStart), c.runs);
    check('F9', `${c.id} coalesced to the latest missed slot`, first.scheduledAtMs === expected.slot && first.coalesced === expected.folded, {first, expected});
    if (expected.late) check('F10', `${c.id} missed slot beyond tolerance is an explicit skip`, first.state === 'skipped' && /lateness tolerance/i.test(first.detail), first);
    else check('F9', `${c.id} missed slot within tolerance is not skipped`, first.state !== 'skipped', first);
  }
  check('F9', 'restart covered a coalesced run within tolerance', Object.values(outcomes).some(o => !o.expected.late && o.expected.folded >= 1), outcomes);
  check('F10', 'restart covered a slot beyond tolerance', Object.values(outcomes).some(o => o.expected.late), outcomes);
  await waitFor(subjects, 'post-restart deliveries and reconciliation', I + 240_000, () => {
    const delivered = c => c.runs.some(r => r.claimedAtMs >= launch.launchStart && r.state === 'completed');
    const interrupted = inflight.runs.find(r => r.id === held.id);
    guard('F11', `${inflight.id} interrupted run is Unknown, never Completed or replayed`, !terminal(interrupted?.state) || interrupted.state === 'unknown', interrupted);
    return delivered(coalesce) && delivered(skip) && interrupted?.state === 'unknown';
  });
  for (const c of [coalesce, skip]) await verifyRun(c, c.runs.find(r => r.claimedAtMs >= launch.launchStart && r.state === 'completed'));
  const reconciled = inflight.runs.find(r => r.id === held.id);
  const blocked = inflight.runs.filter(r => r.claimedAtMs >= launch.launchStart);
  check('F11', `${inflight.id} later slots are held while delivery is unknown`, blocked.every(r => r.state === 'skipped' && /unknown|active/i.test(r.detail)), blocked);
  const heldTranscript = await transcript(inflight.sourceChatId);
  check('F11', `${inflight.id} uncertain message was never resent`, heldTranscript.filter(e => e.id === held.messageId).length === 1 && sentinelLines(inflight).length === 0, {lines: sentinelLines(inflight)});
  attentionCheck(inflight, await attention(), 'automationFailed', reconciled);
  // Resolve the uncertain run without resending it; only a later slot may run.
  const record = await recordOf(inflight.automationId);
  const resolved = await mutate(`${inflight.id}-resolve`, inflight.automationId, record.revision, {kind: 'resolveUnknown', occurrenceId: held.id});
  await waitFor(subjects, `${inflight.id} next slot after Resolve`, I + 240_000, () => inflight.runs.some(r => r.claimedAtMs > resolved.committedAtMs && r.state === 'completed'));
  check('F11', `${inflight.id} resolved run records no delivery`, inflight.runs.find(r => r.id === held.id)?.state === 'resolved', inflight.runs.find(r => r.id === held.id));
  const fresh = inflight.runs.find(r => r.claimedAtMs > resolved.committedAtMs && r.state === 'completed');
  check('F11', `${inflight.id} post-Resolve run is a new occurrence`, fresh.id !== held.id && fresh.messageId !== held.messageId, fresh);
  await verifyRun(inflight, fresh);
  for (const c of Object.values(cases).filter(c => !subjects.includes(c) && c.pausedAtMs)) {
    const runs = await runsOf(c.automationId);
    check('F8', `${c.id} paused definition stayed quiet across restart`, runs.every(r => r.claimedAtMs < c.pausedAtMs), runs);
  }
  for (const c of subjects) await settle(c, 'restart');
  const snapshot = await attention();
  for (const c of subjects) settledAttention(c, snapshot);
  save('restart-readback', {quit, launch, due, outcomes, subjects, attention: snapshot});
}

async function phaseFailure() {
  phase = 'failure';
  const calls = () => {
    const log = path.join(project, 'fixture', 'tool-calls.log');
    return fs.existsSync(log) ? fs.readFileSync(log, 'utf8').split('\n').filter(Boolean) : [];
  };
  const tools = [];
  const base = Date.now() + 40_000;
  for (const [index, provider] of ['codex', 'claude-code'].entries()) {
    const id = `tool-${provider}`, marker = `SW13_TOOL_${hex(4)}`;
    const prompt = [
      `SW-13 tool-failure QA (${id}). Run exactly this command once; it is a disposable local fixture and is expected to fail:`,
      `sh '${path.join(project, 'fixture', 'tool-fail.sh')}' '${id}'`,
      `Do not retry it, inspect it, change files or run anything else. Then reply with exactly ${marker}_TOOL_FAILED if it failed, or ${marker}_TOOL_OK if it succeeded.`,
    ].join('\n');
    tools.push(await newCase(id, provider, 'full', 'existing', {trigger: {kind: 'once', atMs: base + index * 6_000}, prompt, marker, fixture: 'tool-fail.sh', notificationPolicy: 'allRuns'}));
  }
  await waitFor(tools, 'tool-failure occurrences finished', 240_000, () => tools.every(c => c.runs.some(r => terminal(r.state))));
  const toolAttention = await attention();
  for (const c of tools) {
    const run = c.runs.find(r => terminal(r.state));
    check('F12', `${c.id} a delivered turn with a failing tool is not recorded as undelivered`, run.state === 'completed' || run.state === 'unknown', run);
    check('F12', `${c.id} the fixture tool actually ran`, calls().some(line => line.startsWith(`${c.id} `)), calls());
    check('F12', `${c.id} recorded no sentinel effect`, !fs.existsSync(c.sentinel), c.sentinel);
    attentionCheck(c, toolAttention, run.state === 'completed' ? 'automationCompleted' : 'automationFailed', run);
    if (run.state !== 'completed') { observe('F12', `${c.id} provider ended the turn without proven success`, run); continue; }
    const proof = await verifyRun(c, run);
    const failing = proof.tools.filter(t => t.call?.kind === 'exec' && String(t.call.command ?? '').includes('tool-fail.sh'));
    check('F12', `${c.id} history keeps the failing tool call`, failing.some(t => t.isError || /exit 7|simulated error|exit code 7/i.test(String(t.output ?? ''))), failing);
    if (!failing.some(t => t.isError)) observe('F12', `${c.id} the failing exec part is not flagged isError`, failing);
    if (!(proof.finalText.includes(`${c.marker}_TOOL_FAILED`) && !proof.finalText.includes(`${c.marker}_TOOL_OK`))) observe('F12', `${c.id} agent reply did not report the failure as asked`, proof.finalText.slice(0, 400));
    const repeated = calls().filter(line => line.startsWith(`${c.id} `)).length;
    if (repeated > 1) observe('F12', `${c.id} agent ran the failing fixture ${repeated} times in one occurrence`, repeated);
  }
  // Auth-style: LUNA's provider account selection is disconnected in this QA
  // data directory only. Provider credentials stay untouched.
  const auths = [];
  const authBase = Date.now() + 90_000;
  for (const [index, provider] of ['codex', 'claude-code'].entries()) {
    auths.push(await newCase(`auth-${provider}`, provider, 'full', 'existing', {trigger: interval(authBase + index * 6_000), sleepS: 2, notificationPolicy: 'importantUpdates'}));
  }
  for (const c of auths) {
    const before = await ipc.call('ListAgentAccounts', {harness: c.provider}, 90_000);
    const selected = (before.accounts ?? []).find(a => a.harness === c.provider && a.active);
    check('F13', `${c.provider} has an active LUNA account to disconnect`, selected, (before.accounts ?? []).map(a => ({id: a.id, harness: a.harness, active: a.active})));
    disconnected.set(c.provider, selected.id);
    await accountChange('ForgetAgentAccount', c.provider, selected.id);
  }
  progress('provider accounts disconnected in the QA app', {providers: [...disconnected.keys()]});
  await waitFor(auths, 'auth-style occurrences ended', 90_000 + 6_000 + 240_000, () => auths.every(c => c.runs.some(r => terminal(r.state))));
  const failed = {};
  const snapshot = await attention();
  for (const c of auths) {
    const run = c.runs.find(r => terminal(r.state));
    failed[c.id] = run;
    check('F13', `${c.id} a disconnected account is Failed or Unknown, never Completed`, run.state === 'failed' || run.state === 'unknown', run);
    attentionCheck(c, snapshot, 'automationFailed', run);
    const entries = await transcript(c.sourceChatId);
    check('F13', `${c.id} failed message appears at most once and got no reply`, entries.filter(e => e.id === run.messageId).length <= 1
      && !entries.some(e => e.role === 'assistant' && textOf(e).includes(c.marker)), entries.filter(e => e.id === run.messageId));
    check('F13', `${c.id} no effect while disconnected`, sentinelLines(c).length === 0, sentinelLines(c));
    save(`failure-${c.id}-disconnected`, {run, receipt: await receiptOf(run.commandId), transcript: entries});
  }
  for (const [provider, accountId] of [...disconnected]) { await accountChange('ActivateAgentAccount', provider, accountId); disconnected.delete(provider); }
  const reconnectedAt = Date.now();
  progress('provider accounts reconnected');
  await waitFor(auths, 'first slot after reconnect', I + 240_000, () => auths.every(c => c.runs.some(r => r.claimedAtMs > reconnectedAt && terminal(r.state))));
  for (const c of auths) {
    const next = c.runs.filter(r => r.claimedAtMs > reconnectedAt && terminal(r.state)).sort((a, b) => a.claimedAtMs - b.claimedAtMs)[0];
    const first = failed[c.id];
    check('F13', `${c.id} later slot is a new occurrence, never a resend`, next.id !== first.id && next.messageId !== first.messageId, next);
    check('F13', `${c.id} later slot is delivered or explicitly held`, next.state === 'completed' || (next.state === 'skipped' && /active|queued|unresolved/i.test(next.detail)), next);
    if (next.state === 'completed') await verifyRun(c, next);
    else observe('F13', `${c.id} conversation still held after reconnect`, next);
    const entries = await transcript(c.sourceChatId);
    check('F13', `${c.id} the failed message was never delivered later`, entries.filter(e => e.id === first.messageId).length <= 1
      && !entries.some(e => e.role === 'assistant' && textOf(e).includes(c.marker) && e.createdAt < next.claimedAtMs), entries.filter(e => e.id === first.messageId));
  }
  for (const c of auths) await settle(c, 'failure');
  const after = await attention();
  for (const c of [...tools, ...auths]) settledAttention(c, after);
  save('failure-readback', {tools, auths, failed, toolCalls: calls(), attention: after});
}

// ---------------------------------------------------------------- main
async function cleanup() {
  if (!ipc || ipc.closed) return;
  for (const [provider, accountId] of [...disconnected]) {
    try { await accountChange('ActivateAgentAccount', provider, accountId); disconnected.delete(provider); } catch (error) { event('cleanup-error', {provider, error: String(error)}); }
  }
  for (const automationId of created) {
    try {
      const record = await recordOf(automationId);
      if (record?.enabled && !record.deleted) await mutate(`cleanup-pause-${automationId.slice(0, 8)}`, automationId, record.revision, {kind: 'pause'});
    } catch (error) { event('cleanup-error', {automationId, error: String(error)}); }
  }
}
function versions() {
  const run = (cmd, args) => { const r = spawnSync(cmd, args, {encoding: 'utf8', timeout: 15_000}); return r.status === 0 ? r.stdout.trim() : null; };
  return {
    git: run('git', ['-C', root, 'rev-parse', 'HEAD']), dirtyFiles: (run('git', ['-C', root, 'status', '--porcelain']) ?? '').split('\n').filter(Boolean).length,
    codex: run('codex', ['--version']), claude: run('claude', ['--version']), node: process.version,
    bundleVersion: cfg.plist.CFBundleVersion ?? null, executableSha256: sha256(fs.readFileSync(cfg.executable)),
    runnerSha256: sha256(fs.readFileSync(import.meta.filename)),
  };
}
function hashEvidence() {
  const files = {};
  const walk = dir => {
    for (const entry of fs.readdirSync(dir, {withFileTypes: true}).sort((a, b) => a.name.localeCompare(b.name))) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) { if (entry.name !== '.git') walk(full); }
      else if (entry.isFile() && full !== path.join(evidence, 'sha256.json')) files[path.relative(evidence, full)] = sha256(fs.readFileSync(full));
    }
  };
  walk(evidence);
  fs.writeFileSync(path.join(evidence, 'sha256.json'), `${JSON.stringify(files, null, 2)}\n`);
  return Object.keys(files).length;
}

let exitCode = 0;
let failure = null;
try {
  singleQaApp('preflight', true);
  const pid = verifiedListener();
  const info = await connect(pid);
  host = info.deviceId;
  save('manifest', {identity: {appName: cfg.appName, bundleId: cfg.bundleId, port: cfg.port, dataDir: cfg.dataDir, pid, deviceId: host, profileId: info.profileId},
    phases: cfg.phases, intervalMinutes: cfg.intervalMinutes, deadlineMinutes: cfg.deadlineMinutes, startedAt: new Date(startedAt).toISOString(), versions: versions(), engine: info});
  progress('preflight passed', {pid, phases: cfg.phases, evidence});
  await setup();
  if (cfg.phases.includes('runs')) await phaseRuns();
  if (cfg.phases.includes('lifecycle')) await phaseLifecycle();
  if (cfg.phases.includes('restart')) await phaseRestart();
  if (cfg.phases.includes('failure')) await phaseFailure();
} catch (error) {
  exitCode = error instanceof Refusal ? 2 : 1;
  failure = {phase, error: String(error.message ?? error), stack: error.stack};
  save(`failure-${phase}`, failure);
  progress('failed', {error: failure.error});
} finally {
  const failedPhase = phase;
  phase = 'cleanup';
  try { await cleanup(); } catch (error) { event('cleanup-error', {error: String(error)}); }
  try {
    if (ipc && !ipc.closed) save('final-readback', {
      records: await Promise.all([...created].map(id => recordOf(id).catch(e => ({id, error: String(e)})))),
      runs: Object.fromEntries(await Promise.all(Object.values(cases).map(async c => [c.id, await runsOf(c.automationId).catch(e => String(e))]))),
      attention: await attention().catch(e => String(e)),
    });
  } catch (error) { event('cleanup-error', {error: String(error)}); }
  releaseAppLock();
  let qaAppPid = null;
  try { qaAppPid = listener(cfg.port); } catch {}
  const passed = exitCode === 0 && checks.every(c => c.pass);
  save('result', {passed, exitCode, failedPhase: failure ? failedPhase : null, failure, elapsedS: elapsed(), checks, observations,
    summary: {checks: checks.length, failed: checks.filter(c => !c.pass).length, observations: observations.length},
    accountsLeftDisconnected: [...disconnected.keys()], qaAppPid,
    note: qaAppPid === null ? 'The QA app is not running. Relaunch it and pause any SW-13 automation still enabled before other QA.' : null});
  const hashed = hashEvidence();
  clearInterval(heartbeat);
  ipc?.close();
  console.log(JSON.stringify({passed, exitCode, evidence, checks: checks.length, failed: checks.filter(c => !c.pass).length, observations: observations.length, hashedFiles: hashed, elapsedS: elapsed()}));
  process.exitCode = exitCode;
}
