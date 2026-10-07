// Shared plumbing for the OpenCode in-app QA drivers (qa-opencode-live.mjs,
// qa-opencode-scenarios.mjs, qa-opencode-diff.mjs). Contract and failure
// cases QF-09..QF-24: docs/reference/qa-infrastructure.md, "OpenCode QA".
//
// Everything that writes goes through the RPCs the desktop UI uses:
// ExecuteTaskCommand intents built with the UI's fences
// (crates/ui/src/task_commands.rs), approval and form answers mapped like
// crates/ui/src/composer.rs, history review tokens built like
// crates/ui/src/shell.rs, side chats through the side-chat RPCs. State comes
// from persistent subscriptions (WatchDocMessages, WatchSessions, WatchQueue,
// WatchTaskCommands, WatchNativeTasks); single reads only at milestones.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import {EventEmitter} from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import {execFileSync, spawn, spawnSync} from 'node:child_process';

export const root = path.resolve(import.meta.dirname, '..');
export const PRIMARY_PORT = 59777;
export const NO_EXPIRY_FLOOR = 9e18; // APPROVAL_NO_EXPIRY is i64::MAX
export const now = () => new Date().toISOString();
export const sha256 = data => crypto.createHash('sha256').update(data).digest('hex');
export const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
export const inside = (child, parent) => {
  const rel = path.relative(parent, child);
  return rel !== '' && !rel.startsWith('..') && !path.isAbsolute(rel);
};

// ------------------------------------------------------------------ identity
// QF-09: the signed OpenCode QA bundle, its plist, its single IPC listener and
// (for OpenCode runs) its isolated profile wrapper, before any write.
export function assertQaIdentity({provider = 'opencode'} = {}) {
  const identity = {
    appName: process.env.LUNA_DEV_APP_NAME ?? 'LUNA QA OpenCode',
    bundleId: process.env.LUNA_DEV_BUNDLE_ID ?? 'com.novnski.luna.qa.opencode',
    port: Number(process.env.LUNA_DEV_IPC_PORT ?? 59840),
    dataDir: path.resolve(process.env.LUNA_DEV_DATA_DIR ?? path.join(root, 'target/luna-qa-data/opencode')),
  };
  assert.notEqual(identity.port, PRIMARY_PORT, 'refusing the primary LUNA Dev IPC port');
  assert.notEqual(identity.bundleId, 'com.novnski.luna.dev', 'refusing the primary LUNA Dev bundle');
  assert.notEqual(identity.appName, 'LUNA Dev', 'refusing the primary LUNA Dev app name');
  assert.match(identity.bundleId, /^com\.novnski\.luna\.qa\.[a-z][a-z0-9-]*$/);
  const bundle = path.join(root, 'target/luna-macos-dev', `${identity.appName}.app`);
  execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
  const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], {encoding: 'utf8'}));
  const lsenv = plist.LSEnvironment ?? {};
  assert.equal(plist.CFBundleIdentifier, identity.bundleId, 'bundle id mismatch');
  assert.equal(lsenv.LUNA_IPC_PORT, String(identity.port), 'plist IPC port mismatch');
  assert.equal(path.resolve(lsenv.LUNA_DATA_DIR ?? ''), identity.dataDir, 'plist data dir mismatch');
  assert.equal(lsenv.LUNA_QA_MCP_ISOLATION, '1', 'QA bundle must isolate user MCP servers');
  let profile = null;
  if (provider === 'opencode') {
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
  // One Luna GUI at a time (AGENTS.md): any other running bundle is a refusal.
  const guis = execFileSync('ps', ['-axo', 'pid=,comm='], {encoding: 'utf8'}).split('\n')
    .map(line => line.trim()).filter(line => /\.app\/Contents\/MacOS\/luna$/.test(line));
  assert.equal(guis.length, 1, `exactly one Luna GUI may run:\n${guis.join('\n')}`);
  return {...identity, bundle, lsenv, profile, pid, executable, rehearsal: false, wireDir: lsenv.LUNA_OPENCODE_WIRE_LOG ?? null};
}

// QF-21: a rehearsal talks only to a fake engine this process spawned.
export function assertRehearsalIdentity(child, port) {
  assert.ok(child?.pid, 'rehearsal needs the spawned fake engine');
  assert.notEqual(port, PRIMARY_PORT, 'refusing the primary LUNA Dev IPC port');
  const listeners = [...new Set(execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], {encoding: 'utf8'}).match(/^p\d+$/gm) ?? [])];
  assert.deepEqual(listeners, [`p${child.pid}`], 'rehearsal listener is not the fake engine this runner spawned');
  return {appName: 'fake-engine (rehearsal)', bundleId: null, port, dataDir: null, bundle: null, lsenv: {}, profile: null,
    pid: child.pid, executable: 'scripts/qa-opencode-fake-engine.mjs', rehearsal: true, wireDir: null};
}

export async function spawnFakeEngine(extraArgs = []) {
  const child = spawn(process.execPath, [path.join(root, 'scripts/qa-opencode-fake-engine.mjs'), '--port', '0', ...extraArgs], {stdio: ['pipe', 'pipe', 'inherit']});
  const port = await new Promise((resolve, reject) => {
    let buffer = '';
    const timer = setTimeout(() => reject(new Error('fake engine did not report a port')), 10000);
    child.stdout.on('data', chunk => {
      buffer += chunk;
      const line = buffer.split('\n').find(l => l.startsWith('{'));
      if (line) { clearTimeout(timer); resolve(JSON.parse(line).port); }
    });
    child.on('exit', code => reject(new Error(`fake engine exited ${code}`)));
  });
  return {child, port};
}

// ------------------------------------------------------------------ RPC
export class UnknownOutcome extends Error {
  constructor(message) { super(message); this.unknown = true; }
}

// Newline-delimited JSON over the engine's WebSocket: {id, method, params} ->
// {id, ok} | {id, err} | {id, item}... ; {id, cancel: true} ends a stream.
export class Rpc {
  constructor({port, log = () => {}}) {
    this.port = port;
    this.log = log;
    this.seq = 0;
    this.handlers = new Map();
    this.subscriptions = [];
  }

  async connect() {
    this.ws = new WebSocket(`ws://127.0.0.1:${this.port}`);
    await new Promise((resolve, reject) => {
      this.ws.addEventListener('open', resolve, {once: true});
      this.ws.addEventListener('error', () => reject(new Error(`cannot connect to ws://127.0.0.1:${this.port}`)), {once: true});
    });
    this.ws.addEventListener('message', event => {
      for (const line of String(event.data).split('\n').filter(Boolean)) {
        let frame;
        try { frame = JSON.parse(line); } catch { continue; }
        this.handlers.get(frame.id)?.(frame);
      }
    });
    this.ws.addEventListener('close', () => {
      for (const sub of this.subscriptions) if (!sub.record.closedAt) sub.record.error ??= 'socket closed';
    });
    return this;
  }

  call(method, params = {}, {timeoutMs = 45000} = {}) {
    const started = Date.now();
    return new Promise((resolve, reject) => {
      const id = ++this.seq;
      const timer = setTimeout(() => {
        this.handlers.delete(id);
        try { this.ws.send(JSON.stringify({id, cancel: true})); } catch {}
        this.log({method, params, durationMs: Date.now() - started, outcome: 'timeout'});
        reject(new UnknownOutcome(`${method} timed out after ${timeoutMs} ms; outcome unknown, never replayed`));
      }, timeoutMs);
      this.handlers.set(id, frame => {
        if (!['ok', 'err', 'item'].some(key => Object.hasOwn(frame, key))) return;
        clearTimeout(timer);
        this.handlers.delete(id);
        if (Object.hasOwn(frame, 'item')) this.ws.send(JSON.stringify({id, cancel: true}));
        const durationMs = Date.now() - started;
        if (Object.hasOwn(frame, 'err')) {
          this.log({method, params, durationMs, outcome: 'err', error: frame.err});
          reject(Object.assign(new Error(`${method}: ${JSON.stringify(frame.err)}`), {rpcError: frame.err}));
        } else {
          this.log({method, params, durationMs, outcome: 'ok'});
          resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
        }
      });
      this.ws.send(JSON.stringify({id, method, params}));
    });
  }

  // Persistent subscription (validation-plan §2 item 6); never re-polled.
  subscribe(method, params, onItem, onError = () => {}) {
    const id = ++this.seq;
    const record = {method, params, id, subscribedAt: now(), frames: 0, firstFrameAt: null, lastFrameAt: null, closedAt: null, error: null};
    const sub = {record, close: () => {
      if (record.closedAt) return;
      record.closedAt = now();
      this.handlers.delete(id);
      try { this.ws.send(JSON.stringify({id, cancel: true})); } catch {}
    }};
    this.subscriptions.push(sub);
    this.handlers.set(id, frame => {
      if (Object.hasOwn(frame, 'err')) { record.error = frame.err; onError(frame.err); return; }
      if (!Object.hasOwn(frame, 'item')) return;
      record.frames += 1;
      record.firstFrameAt ??= now();
      record.lastFrameAt = now();
      onItem(frame.item);
    });
    this.log({method, params, outcome: 'subscribed'});
    this.ws.send(JSON.stringify({id, method, params}));
    return sub;
  }

  close() {
    for (const sub of this.subscriptions) sub.close();
    try { this.ws?.close(); } catch {}
  }
}

// ------------------------------------------------------------------ transcript
// Applies TranscriptUpdate frames exactly like apply_transcript_frame
// (crates/doc/src/transcript_delta.rs). A desync resubscribes for a reset.
export function applyTranscriptFrame(entries, update) {
  const frame = update?.frame ?? update ?? {};
  if (Array.isArray(frame.reset)) return {entries: frame.reset.map(e => structuredClone(e))};
  const next = entries.map(e => e);
  if (Array.isArray(frame.remove) && frame.remove.length) {
    const gone = new Set(frame.remove);
    for (let i = next.length - 1; i >= 0; i--) if (gone.has(next[i].id)) next.splice(i, 1);
  }
  for (const {after, entry} of frame.upsert ?? []) {
    const existing = next.findIndex(e => e.id === entry.id);
    if (existing >= 0) next.splice(existing, 1);
    let at = 0;
    if (after != null) {
      const anchor = next.findIndex(e => e.id === after);
      if (anchor < 0) return {entries, desync: `missing anchor ${after}`};
      at = anchor + 1;
    }
    next.splice(at, 0, structuredClone(entry));
  }
  for (const {entry, part, text, len} of frame.append ?? []) {
    const index = next.findIndex(e => e.id === entry);
    if (index < 0) return {entries, desync: `missing append entry ${entry}`};
    const target = structuredClone(next[index]);
    const tail = target.parts.find(p => (p.kind === 'text' || p.kind === 'reasoning') && p.id === part);
    if (!tail) return {entries, desync: `missing append part ${entry}/${part}`};
    tail.text += text;
    if (Buffer.byteLength(tail.text, 'utf8') !== len) return {entries, desync: `append length ${Buffer.byteLength(tail.text, 'utf8')} != ${len}`};
    next[index] = target;
  }
  if (typeof frame.count === 'number' && next.length !== frame.count) return {entries, desync: `count ${next.length} != ${frame.count}`};
  return {entries: next};
}

export const textOf = entry => (entry?.parts ?? []).filter(p => p.kind === 'text').map(p => p.text).join('\n');
// The UI's reviewed message text: text parts trimmed, empties dropped, "\n".
export const reviewedText = entry => (entry?.parts ?? []).filter(p => p.kind === 'text').map(p => p.text.trim()).filter(Boolean).join('\n');

export class TranscriptMirror extends EventEmitter {
  constructor(rpc, chatId, {label = chatId, record = () => {}} = {}) {
    super();
    this.rpc = rpc;
    this.chatId = chatId;
    this.label = label;
    this.record = record;
    this.entries = [];
    this.updates = 0;
    this.desyncs = [];
    this.latest = {contextUsage: null, providerNotice: null, contextControl: null, checkpointed: null};
    this.resets = 0;
    this.sub = null;
    this.ready = new Promise(resolve => { this.markReady = resolve; });
  }

  start() {
    this.sub = this.rpc.subscribe('WatchDocMessages', {chatId: this.chatId}, item => this.onItem(item), err => this.emit('error', err));
    return this;
  }

  onItem(item) {
    const result = applyTranscriptFrame(this.entries, item);
    const frame = item?.frame ?? item ?? {};
    if (result.desync) {
      this.desyncs.push({at: now(), reason: result.desync});
      this.record({at: now(), chat: this.label, kind: 'desync', reason: result.desync});
      this.sub.close();
      this.start(); // a fresh subscription starts with a reset (QF-15)
      return;
    }
    this.entries = result.entries;
    this.updates += 1;
    if (Array.isArray(frame.reset)) this.resets += 1;
    for (const key of ['contextUsage', 'providerNotice', 'contextControl', 'checkpointed']) if (item && Object.hasOwn(item, key) && item[key] !== undefined) this.latest[key] = item[key];
    this.record({at: now(), chat: this.label, kind: Array.isArray(frame.reset) ? 'reset' : 'delta',
      entries: this.entries.length, upsert: frame.upsert?.length ?? 0, append: frame.append?.length ?? 0, remove: frame.remove?.length ?? 0,
      providerNotice: item?.providerNotice ?? null, contextUsage: item?.contextUsage ?? null, contextControl: item?.contextControl ?? null});
    this.markReady();
    this.emit('change', this);
  }

  pendingInputs() {
    const out = [];
    for (const entry of this.entries) {
      if (entry.role !== 'assistant') continue;
      for (const part of entry.parts ?? []) {
        if (part.kind === 'input' && !part.resolved) out.push({entryId: entry.id, partId: part.id, requestId: part.requestId, questions: part.questions ?? []});
      }
    }
    return out;
  }

  // The request the UI's panel shows (pending_input_request).
  currentPending() {
    const assistants = this.entries.filter(e => e.role === 'assistant');
    const latest = assistants.at(-1);
    const fromLatest = latest?.parts?.find(p => p.kind === 'input' && !p.resolved);
    if (fromLatest) return {entryId: latest.id, partId: fromLatest.id, requestId: fromLatest.requestId, questions: fromLatest.questions ?? []};
    for (const entry of [...assistants].reverse()) {
      const part = entry.parts?.find(p => p.kind === 'input' && !p.resolved && (p.questions ?? []).some(q => q.mcp));
      if (part) return {entryId: entry.id, partId: part.id, requestId: part.requestId, questions: part.questions ?? []};
    }
    return null;
  }

  inputResolved(requestId) {
    return this.entries.some(e => (e.parts ?? []).some(p => p.kind === 'input' && p.requestId === requestId && p.resolved));
  }

  waitFor(predicate, {timeoutMs = 120000, label = 'transcript condition'} = {}) {
    return waitOn([this], () => predicate(this.entries, this), timeoutMs, label);
  }

  close() { this.sub?.close(); }
}

// Resolves when check() returns a truthy value, re-evaluated on each 'change'
// of the given emitters (frames), never by issuing RPCs.
export function waitOn(emitters, check, timeoutMs, label) {
  return new Promise((resolve, reject) => {
    let done = false;
    const finish = (fn, value) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      for (const e of emitters) e.off('change', evaluate);
      fn(value);
    };
    const evaluate = () => {
      let value;
      try { value = check(); } catch (error) { finish(reject, error); return; }
      if (value) finish(resolve, value);
    };
    const timer = setTimeout(() => finish(reject, Object.assign(new Error(`timed out waiting for ${label} (${timeoutMs} ms)`), {timeout: true})), timeoutMs);
    for (const e of emitters) e.on('change', evaluate);
    evaluate();
  });
}

export class SessionWatch extends EventEmitter {
  constructor(rpc, record = () => {}) {
    super();
    this.rows = new Map();
    this.history = new Map(); // chatId -> [{seq, at, status, lastCompletedTurn}]
    this.seq = 0;
    this.record = record;
    this.sub = rpc.subscribe('WatchSessions', {}, item => {
      const rows = Array.isArray(item) ? item : item?.sessions ?? [];
      for (const row of rows) {
        const prev = this.rows.get(row.chatId);
        this.rows.set(row.chatId, row);
        if (!prev || prev.status !== row.status || prev.lastCompletedTurn !== row.lastCompletedTurn) {
          const list = this.history.get(row.chatId) ?? [];
          list.push({seq: ++this.seq, at: now(), status: row.status, lastCompletedTurn: row.lastCompletedTurn ?? null});
          this.history.set(row.chatId, list);
          this.record({at: now(), chatId: row.chatId, status: row.status, lastCompletedTurn: row.lastCompletedTurn ?? null, recovery: row.recovery ?? null});
        }
      }
      this.emit('change', this);
    });
  }
  status(chatId) { return this.rows.get(chatId)?.status ?? null; }
  mark(chatId) { return {seq: this.seq, lastCompletedTurn: this.rows.get(chatId)?.lastCompletedTurn ?? null}; }
  since(chatId, mark) { return (this.history.get(chatId) ?? []).filter(h => h.seq > mark.seq); }
  statusesSince(chatId, mark) { return this.since(chatId, mark).map(h => h.status); }
  close() { this.sub.close(); }
}

export class StreamWatch extends EventEmitter {
  constructor(rpc, method, params, record = () => {}) {
    super();
    this.frames = [];
    this.latest = null;
    this.sub = rpc.subscribe(method, params, item => {
      this.latest = item;
      this.frames.push({at: now(), item});
      record({at: now(), method, params, item});
      this.emit('change', this);
    }, err => { this.error = err; this.emit('change', this); });
  }
  close() { this.sub.close(); }
}

// WatchTaskCommands: {kind: snapshot, read} then {kind: receipt, receipt}.
export class ReceiptWatch extends EventEmitter {
  constructor(rpc, taskId, record = () => {}) {
    super();
    this.taskId = taskId;
    this.receipts = new Map();
    this.snapshot = null;
    this.sub = rpc.subscribe('WatchTaskCommands', {taskId}, item => {
      if (item?.kind === 'snapshot') {
        this.snapshot = item.read?.snapshot ?? null;
        for (const event of item.read?.events ?? []) if (event.receipt) this.receipts.set(event.receipt.commandId, event.receipt);
      } else if (item?.kind === 'receipt' && item.receipt) {
        this.receipts.set(item.receipt.commandId, item.receipt);
        record({at: now(), taskId, receipt: summarizeReceipt(item.receipt)});
      }
      this.emit('change', this);
    });
  }
  close() { this.sub.close(); }
}

export const summarizeReceipt = r => r && ({commandId: r.commandId, intent: r.intent, status: r.status, reasonCode: r.reason?.code ?? null, reason: r.reason?.message ?? null, recoveryAction: r.recoveryAction ?? null});
export const terminalReceipt = r => r && r.status !== 'pending';

// ------------------------------------------------------------------ fences
// Copies of crates/ui/src/task_commands.rs revision_fenced_kind /
// binding_fenced_kind and the turn fence of execute_receipt_on_with_snapshot.
const REVISION_FENCED = new Set(['nativeReview', 'rename', 'setPinned', 'setRead', 'setSettled', 'markSeen', 'moveProject', 'setArchived', 'delete', 'setBranch', 'setCwd', 'setHost', 'setConfig', 'setModel', 'goal', 'compactContext', 'clearContext', 'switchProvider', 'setPermission', 'setProviderAccount', 'forkTask', 'editEarlierMessage', 'restoreHistoryRecovery', 'rewindConversation', 'reconcileConversationRewind', 'applyContextTransfer']);
const BINDING_FENCED = new Set(['session', 'nativeReview', 'queueMessage', 'editQueuedMessage', 'removeQueuedMessage', 'sendQueuedNow', 'steerQueuedNow', 'retryRecovery', 'cancelRecovery', ...REVISION_FENCED]);

export function taskCommandParams({taskId, commandId, intent, snapshot, actorId, clientSessionId}) {
  const kind = intent.kind;
  const control = intent.command?.kind;
  const turnFenced = (kind === 'session' && ['steer', 'interrupt'].includes(control)) || ['sendQueuedNow', 'steerQueuedNow'].includes(kind);
  const revisionFenced = REVISION_FENCED.has(kind) || (kind === 'session' && intent.command?.request?.review != null);
  const params = {commandId, actorId, clientId: actorId, clientSessionId, taskId, intent};
  if (revisionFenced) params.expectedTaskRevision = snapshot.taskRevision;
  if (BINDING_FENCED.has(kind) && snapshot.bindingGeneration != null) params.expectedBindingGeneration = snapshot.bindingGeneration;
  if (turnFenced && snapshot.activeTurnId) params.expectedTurnId = snapshot.activeTurnId;
  return params;
}

// ------------------------------------------------------------------ answers
const CHOICE_SYNONYMS = {
  'allow-once': ['Allow once'],
  'allow-task': ['Allow for task'],
  deny: ['Deny'],
  cancel: ['Cancel turn', 'Cancel'],
};
const ENUM_DECISION = {'allow-once': 'allowOnce', 'allow-task': 'allowTask', deny: 'deny', cancel: 'cancel'};

export class NotAnswerable extends Error {}

export const isLunaCrossTask = question => ['lunaCrossTask', 'luna_cross_task', 'LunaCrossTask'].includes(question?.approval?.actionKind);

// QF-12: exactly approval_decision() in crates/ui/src/composer.rs, minus its
// silent Deny fallback: an absent choice fails instead.
export function approvalDecision(question, action) {
  assert.ok(CHOICE_SYNONYMS[action], `unknown approval action ${action}`);
  const choices = question?.approval?.details?.choices ?? [];
  if (question?.approval?.details) {
    const labels = choices.map(c => c.label);
    const index = labels.findIndex(label => CHOICE_SYNONYMS[action].includes(label));
    if (index < 0) throw new NotAnswerable(`approval card offers [${labels.join(', ')}], not ${action}`);
    return {decision: {providerChoice: {index}}, label: labels[index]};
  }
  if (action === 'allow-task' && !question?.approval?.supportsTaskAllowance) throw new NotAnswerable('approval does not support task allowance');
  return {decision: ENUM_DECISION[action], label: CHOICE_SYNONYMS[action][0]};
}

// Answers for a question or MCP form panel. policy:
//   {pick: 'text'|['a','b']} choose options whose label contains the text
//   (else the first option); {text: '...'} typed answer; {dismiss: true}
//   empty answers; {mcp: 'Accept'|'Decline'|'Cancel', values: {field: v}}.
export function buildAnswers(questions, policy = {}) {
  if (policy.dismiss) return questions.map(q => ({questionId: q.id, labels: []}));
  return questions.map(question => {
    const mcp = question.mcp;
    if (mcp && (mcp.kind?.type === 'formAction' || mcp.kind?.type === 'url')) {
      return {questionId: question.id, labels: [policy.mcp ?? 'Accept']};
    }
    if (mcp && mcp.kind?.type === 'field') {
      const {name, schema} = mcp.kind;
      const wanted = policy.values?.[name];
      if (schema.choices?.length) {
        const pick = wanted == null ? [schema.choices.find(c => c.value === schema.default)?.label ?? schema.choices[0].label]
          : [].concat(wanted).map(v => (schema.choices.find(c => c.value === v || c.label === v) ?? schema.choices[0]).label);
        return {questionId: question.id, labels: schema.valueType === 'array' ? pick : pick.slice(0, 1)};
      }
      const value = wanted ?? schema.default ?? ({string: 'qa', integer: 1, number: 1, boolean: true})[schema.valueType];
      return {questionId: question.id, labels: value == null ? [] : [typeof value === 'string' ? value : JSON.stringify(value)]};
    }
    if (policy.text != null && (!question.options?.length || question.input?.allowOther)) return {questionId: question.id, labels: [policy.text]};
    const options = question.options ?? [];
    if (!options.length) return {questionId: question.id, labels: [policy.text ?? 'qa answer']};
    const wanted = [].concat(policy.pick ?? []).map(s => String(s).toLowerCase());
    const matches = options.filter(o => wanted.some(w => o.toLowerCase().includes(w)));
    const labels = matches.length ? (question.multiSelect ? matches : matches.slice(0, 1)) : [options[0]];
    return {questionId: question.id, labels};
  });
}

// ------------------------------------------------------------------ evidence
export class Evidence {
  constructor(dir, manifest) {
    this.dir = dir;
    this.manifest = manifest;
    this.criteria = [];
    fs.mkdirSync(dir, {recursive: true});
  }
  file(name) { const f = path.join(this.dir, name); fs.mkdirSync(path.dirname(f), {recursive: true}); return f; }
  save(name, value) { fs.writeFileSync(this.file(name), `${JSON.stringify(value, null, 2)}\n`); }
  append(name, value) { fs.appendFileSync(this.file(name), `${JSON.stringify(value)}\n`); }
  writeManifest() { this.save('manifest.json', this.manifest); }
  command(record) {
    // commands.jsonl: every RPC; large payloads (upload chunks) by size only.
    const params = record.params?.data ? {...record.params, data: `<${record.params.data.length} base64 chars>`} : record.params;
    const text = JSON.stringify(params ?? null);
    this.append('commands.jsonl', {at: now(), ...record, params: text.length > 4096 ? {truncated: true, sha256: sha256(text), bytes: text.length} : params});
  }
  check(criterion, expected, actual, passed, extra = {}) {
    const row = {criterion, expected, actual, passed: Boolean(passed), ...extra};
    this.criteria.push(row);
    const tag = row.informational ? `INFO:${row.passed ? 'yes' : 'no'}` : row.passed ? 'PASS' : 'FAIL';
    console.error(`[${tag}] ${extra.step ? `${extra.step}: ` : ''}${criterion}: ${typeof actual === 'string' ? actual : JSON.stringify(actual)}`);
    return row.passed;
  }
}

// QF-10/QF-17: a send is recorded with outcome "pending" before its RPC.
export function recordSend(evidence, send) {
  const row = {index: evidence.manifest.sends.length, phase: evidence.phase ?? null, attemptedAt: now(), outcome: 'pending', ...send};
  evidence.manifest.sends.push(row);
  evidence.writeManifest();
  return row;
}
export function settleSend(evidence, row, fields) {
  Object.assign(row, fields, {settledAt: now()});
  evidence.writeManifest();
}

// Informational rows (model prose, optional native behaviour) are reported
// but never decide the result.
export function finishEvidence(evidence, {title, tier, rehearsal = false, notes = [], unverified = [], writePass = true}) {
  const manifest = evidence.manifest;
  const scanArgs = [path.join(root, 'scripts/qa-opencode-manifest.py'), 'scan', '--dir', evidence.dir];
  manifest.endedAt = now();
  manifest.criteria = evidence.criteria;
  evidence.writeManifest();
  const scan = spawnSync('python3', scanArgs, {encoding: 'utf8'});
  evidence.command({method: 'secret-scan', params: {argv: ['python3', ...scanArgs]}, outcome: scan.status === 0 ? 'ok' : 'err'});
  evidence.check('secret-scan', 'no credentials in evidence (OCF-142)', scan.stdout.trim() || scan.stderr.trim(), scan.status === 0);
  manifest.criteria = evidence.criteria;
  const decisive = evidence.criteria.filter(c => !c.informational);
  manifest.passed = decisive.length > 0 && decisive.every(c => c.passed);
  evidence.writeManifest();
  const cell = value => String(typeof value === 'string' ? value : JSON.stringify(value)).replaceAll('|', '/').replaceAll('\n', ' ').slice(0, 300);
  const rows = evidence.criteria.map(c => `| ${c.step ? `${c.step}: ` : ''}${c.criterion} | ${cell(c.expected)} | ${cell(c.actual)} | ${c.informational ? `info: ${c.passed ? 'yes' : 'no'}` : c.passed ? 'yes' : 'NO'} |`);
  fs.writeFileSync(evidence.file('result.md'), [
    `# ${title}`, '',
    `- Provider: ${manifest.requested?.provider}; model ${manifest.requested?.model ?? '(default)'}; variant ${manifest.requested?.variant ?? '(none)'}; permission ${manifest.requested?.permissionMode ?? '-'}.`,
    `- Evidence tier: **${tier}**${rehearsal ? ' (rehearsal against the fake engine: proves the driver plumbing only, never LUNA or provider behaviour; QF-21)' : ' through the signed QA app\'s RPC surface'}.`,
    `- App: ${manifest.app?.name} pid ${manifest.app?.pid}; LUNA ${manifest.luna?.sha} (dirty=${manifest.luna?.dirty}).`,
    `- Result: **${manifest.passed ? (rehearsal ? 'REHEARSAL OK' : 'PASS') : 'FAIL'}**`, '',
    '| Criterion | Expected | Actual | Pass |', '| --- | --- | --- | --- |', ...rows, '',
    ...(notes.length ? ['## Notes', '', ...notes.map(n => `- ${n}`), ''] : []),
    '## Unverified by this run', '',
    ...unverified.map(u => `- ${u}`),
    '- Model consumption claims; a model statement is not verification (validation-plan §1 item 7).',
    '- Screenshots prove presentation only; RPC admission proves admission, not consumption.', '',
    '## Replay', '', '```', manifest.replay, '```', '',
  ].join('\n'));
  if (manifest.passed && writePass) evidence.save(rehearsal ? 'rehearsal.json' : 'pass.json', {scenario: manifest.scenario, tier, provider: manifest.requested?.provider, passedAt: now()});
  spawnSync('python3', [path.join(root, 'scripts/qa-opencode-manifest.py'), 'sums', '--dir', evidence.dir], {encoding: 'utf8'});
  return manifest.passed;
}

export function lunaSource() {
  const git = (...args) => spawnSync('git', ['-C', root, ...args], {encoding: 'utf8'}).stdout.trim();
  return {sha: git('rev-parse', 'HEAD'), branch: git('rev-parse', '--abbrev-ref', 'HEAD'), dirty: git('status', '--porcelain') !== ''};
}

// ------------------------------------------------------------------ wire log
export function wireOffsets(dir) {
  if (!dir || !fs.existsSync(dir)) return {dir: dir ?? null, present: false, files: {}};
  const files = {};
  for (const name of fs.readdirSync(dir)) {
    const file = path.join(dir, name);
    if (fs.statSync(file).isFile()) files[name] = fs.statSync(file).size;
  }
  return {dir, present: true, files};
}

export function sliceFile(source, start, destination) {
  const size = fs.statSync(source).size;
  const length = Math.max(0, size - start);
  const buffer = Buffer.alloc(length);
  const fd = fs.openSync(source, 'r');
  try { fs.readSync(fd, buffer, 0, length, start); } finally { fs.closeSync(fd); }
  fs.mkdirSync(path.dirname(destination), {recursive: true});
  fs.writeFileSync(destination, buffer);
  return {startOffset: start, endOffset: size, bytes: length, sha256: sha256(buffer)};
}

// QF-11: the run's slice of every opencode-wire-<pid>.jsonl, split into
// native-frames.jsonl (sse*) and http.jsonl (http) exactly as recorded.
export function captureWireSlice(evidence, wireDir, startFiles) {
  const end = wireOffsets(wireDir);
  const slices = {};
  if (end.present) {
    for (const [name, size] of Object.entries(end.files)) {
      const start = startFiles?.[name] ?? 0;
      if (size > start) slices[name] = sliceFile(path.join(wireDir, name), start, evidence.file(path.join('wire', name)));
    }
  }
  let frames = 0;
  let http = 0;
  for (const name of Object.keys(slices)) {
    for (const line of fs.readFileSync(evidence.file(path.join('wire', name)), 'utf8').split('\n').filter(Boolean)) {
      let record;
      try { record = JSON.parse(line); } catch { continue; }
      if (String(record.kind).startsWith('sse')) { fs.appendFileSync(evidence.file('native-frames.jsonl'), `${line}\n`); frames += 1; }
      else if (record.kind === 'http') { fs.appendFileSync(evidence.file('http.jsonl'), `${line}\n`); http += 1; }
    }
  }
  const bytes = Object.values(slices).reduce((sum, s) => sum + s.bytes, 0);
  return {dir: wireDir, present: end.present, start: startFiles, end: end.files, slices, bytes, frames, http};
}

export function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
}

// ------------------------------------------------------------------ capture
// QF-19: the QA listener's own largest layer-0 window; screencapture only
// after the no-prompt Screen Recording probe. AX readback drives System
// Events, which can raise the macOS Automation prompt the first time, so it
// is opt-in (--ax) and bounded.
function jxa(script, timeout = 15000) {
  return execFileSync('osascript', ['-l', 'JavaScript', '-e', script], {encoding: 'utf8', timeout}).trim();
}

export function windowOf(pid) {
  const script = `ObjC.import("CoreGraphics");
const raw = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, $.kCGNullWindowID);
JSON.stringify(ObjC.deepUnwrap(ObjC.castRefToObject(raw)).filter(w => w.kCGWindowOwnerPID === ${Number(pid)} && w.kCGWindowLayer === 0)
  .map(w => ({number: w.kCGWindowNumber, bounds: w.kCGWindowBounds, onscreen: w.kCGWindowIsOnscreen})));`;
  const windows = JSON.parse(jxa(script));
  windows.sort((a, b) => b.bounds.Width * b.bounds.Height - a.bounds.Width * a.bounds.Height);
  return windows[0] ?? null;
}

export function screenRecordingGranted() {
  const script = `ObjC.import("CoreGraphics");
const l = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, $.kCGNullWindowID)));
String(l.some(w => w.kCGWindowLayer === 0 && w.kCGWindowOwnerPID !== ${process.pid} && w.kCGWindowName));`;
  return jxa(script) === 'true';
}

export function axReadback(pid, {maxNodes = 400, timeout = 20000} = {}) {
  // Breadth-first over the frontmost window's AX tree via System Events;
  // records role, subrole, name/description and value text only.
  const script = `const se = Application("System Events");
const proc = se.processes.whose({unixId: ${Number(pid)}})[0];
const out = []; const queue = [];
try { proc.windows().forEach(w => queue.push([w, 0])); } catch (e) { out.push({error: String(e)}); }
while (queue.length && out.length < ${Number(maxNodes)}) {
  const [el, depth] = queue.shift();
  let row = {depth};
  try { row.role = el.role(); } catch (e) {}
  try { row.subrole = el.subrole(); } catch (e) {}
  try { row.name = el.name(); } catch (e) {}
  try { row.description = el.description(); } catch (e) {}
  try { const v = el.value(); if (typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean') row.value = v; } catch (e) {}
  out.push(row);
  try { el.uiElements().forEach(c => queue.push([c, depth + 1])); } catch (e) {}
}
JSON.stringify(out);`;
  return JSON.parse(jxa(script, timeout));
}

export function captureWindow(evidence, pid, label, {ax = false} = {}) {
  const safe = label.replace(/[^a-z0-9._-]+/gi, '-');
  const record = {label, at: now(), pid, window: null, screenshot: null, ax: null, evidence: 'presentation only (QF-19)'};
  try { record.window = windowOf(pid); } catch (error) { record.windowError = String(error.message ?? error); }
  if (!record.window) {
    record.screenshot = {status: 'blocked', reason: record.windowError ?? 'the QA app has no on-screen layer-0 window'};
  } else {
    let granted = false;
    try { granted = screenRecordingGranted(); } catch (error) { record.permissionProbeError = String(error.message ?? error); }
    if (!granted) {
      record.screenshot = {status: 'blocked', reason: 'Screen Recording permission absent for this process (never prompted); fell back to AX/document readback'};
    } else {
      const file = evidence.file(path.join('screenshots', `${safe}.png`));
      const shot = spawnSync('screencapture', ['-x', '-o', `-l${record.window.number}`, file], {encoding: 'utf8', timeout: 20000});
      if (shot.status === 0 && fs.existsSync(file) && fs.statSync(file).size > 0) {
        record.screenshot = {status: 'captured', file: path.relative(evidence.dir, file), bytes: fs.statSync(file).size, sha256: sha256(fs.readFileSync(file))};
      } else {
        record.screenshot = {status: 'blocked', reason: `screencapture exit ${shot.status}: ${(shot.stderr || '').trim()}`};
      }
    }
  }
  if (!ax) {
    record.ax = {status: 'skipped', reason: 'AX readback is opt-in (--ax): System Events may prompt for Automation access'};
  } else {
    try {
      const nodes = axReadback(pid);
      evidence.save(path.join('ax', `${safe}.json`), nodes);
      record.ax = {status: 'captured', nodes: nodes.length, file: `ax/${safe}.json`};
    } catch (error) {
      record.ax = {status: 'blocked', reason: String(error.message ?? error).split('\n')[0]};
    }
  }
  evidence.save(path.join('screenshots', `${safe}.json`), record);
  return record;
}

// ------------------------------------------------------------------ normalize
// QF-20: kinds, layout, statuses and shapes; volatile values dropped.
export function normalizeToolCall(call) {
  if (!call) return null;
  if (call.kind === 'mcp') return `mcp:${call.server}/${call.tool}`;
  if (call.kind === 'unknown') return /^Agent(:|$)/.test(call.name ?? '') ? 'unknown:Agent' : `unknown:${call.name}`;
  return call.kind;
}

export function approvalShape(approval) {
  if (!approval) return null;
  const details = approval.details ?? null;
  const detailKeys = details ? Object.entries(details).filter(([, v]) => v != null && !(Array.isArray(v) && v.length === 0)).map(([k]) => k).sort() : [];
  return {
    actionKind: approval.actionKind, requestedScope: approval.requestedScope, supportsTaskAllowance: approval.supportsTaskAllowance,
    expiry: approval.expiresAtMs >= NO_EXPIRY_FLOOR ? 'none' : 'deadline', hasSummary: Boolean(approval.summary),
    choices: (details?.choices ?? []).map(c => `${c.label}:${c.scope}`), detailKeys,
  };
}

export function questionShape(question) {
  return {
    hasHeader: Boolean(question.header), optionCount: (question.options ?? []).length, options: question.approval ? question.options : undefined,
    multiSelect: Boolean(question.multiSelect), approval: approvalShape(question.approval),
    mcp: question.mcp ? {kind: question.mcp.kind?.type, valueType: question.mcp.kind?.schema?.valueType ?? null, choices: question.mcp.kind?.schema?.choices?.length ?? 0} : null,
    input: question.input ? {allowOther: Boolean(question.input.allowOther), secret: Boolean(question.input.secret)} : null,
  };
}

export function normalizePart(part) {
  switch (part.kind) {
    case 'tool': return {kind: 'tool', tool: normalizeToolCall(part.call), resolved: Boolean(part.resolved), isError: Boolean(part.isError),
      output: part.output != null || part.outputRef != null ? 'present' : 'none', diff: part.diff != null || part.diffRef != null,
      subagent: part.subagentRef ? (part.subagentStatus ?? 'linked') : null};
    case 'input': return {kind: 'input', resolved: Boolean(part.resolved), questions: (part.questions ?? []).map(questionShape)};
    case 'asyncQuestion': return {kind: 'asyncQuestion', resolved: Boolean(part.resolved), count: (part.questions ?? []).length};
    default: return {kind: part.kind};
  }
}

// Layout: role plus part kinds with consecutive text/reasoning runs collapsed.
export function normalizeEntries(entries) {
  return entries.map(entry => {
    const parts = [];
    for (const part of entry.parts ?? []) {
      const n = normalizePart(part);
      const last = parts.at(-1);
      if (last && ['text', 'reasoning'].includes(n.kind) && last.kind === n.kind) continue;
      parts.push(n);
    }
    return {role: entry.role, status: entry.status ?? null, continuation: Boolean(entry.continuationOf), parts};
  });
}

export function toolMultiset(entries) {
  const counts = {};
  for (const entry of entries) for (const part of entry.parts ?? []) if (part.kind === 'tool') {
    const key = `${normalizeToolCall(part.call)}${part.isError ? '!error' : ''}`;
    counts[key] = (counts[key] ?? 0) + 1;
  }
  return counts;
}

export function maskNonce(text, nonce) {
  return nonce ? String(text).replaceAll(nonce, '<nonce>') : String(text);
}

export function makeFixture(prefix = 'luna-qa-opencode-scenario-') {
  // QF-02: under the system temp dir, never inside $HOME. The parent's name
// prefix lets rehearsals remove exactly the fixtures they created.
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  const fixture = path.join(parent, 'fixture');
  assert.ok(!inside(fs.realpathSync(parent), fs.realpathSync(os.homedir())), 'fixture must be outside $HOME (QF-02)');
  fs.mkdirSync(fixture);
  const env = {...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null'};
  const git = (...args) => execFileSync('git', ['-C', fixture, ...args], {encoding: 'utf8', env}).trim();
  git('init', '-q', '-b', 'main');
  git('config', 'user.name', 'OpenCode QA');
  git('config', 'user.email', 'qa@example.invalid');
  fs.writeFileSync(path.join(fixture, 'README.md'), 'Disposable OpenCode QA fixture. Do not modify files unless asked.\n');
  git('add', 'README.md');
  git('commit', '-q', '-m', 'fixture');
  return {dir: fs.realpathSync(fixture), git};
}

export function fileHashes(dir, names) {
  return Object.fromEntries(names.map(name => {
    const file = path.join(dir, name);
    return [name, fs.existsSync(file) ? sha256(fs.readFileSync(file)) : null];
  }));
}

// A small valid PNG (solid RGB) for image scenarios; deterministic bytes.
export function solidPng(width, height, [r, g, b]) {
  const crcTable = Array.from({length: 256}, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = buf => { let c = 0xffffffff; for (const byte of buf) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const sum = Buffer.alloc(4); sum.writeUInt32BE(crc(body));
    return Buffer.concat([len, body, sum]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0); ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; ihdr[9] = 2; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  const row = Buffer.concat([Buffer.from([0]), Buffer.alloc(width * 3).fill(Buffer.from([r, g, b]))]);
  const raw = Buffer.concat(Array.from({length: height}, () => row));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
