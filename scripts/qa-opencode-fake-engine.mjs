#!/usr/bin/env node
// Rehearsal-only fake LUNA engine for scripts/qa-opencode-scenarios.mjs
// (`rehearse`, tier R0, QF-21). It speaks the engine's WebSocket framing and
// the RPC shapes the scenario driver uses, with a scripted agent, so the
// runner's plumbing (RPC payloads, fences, waits, answers, evidence, diff)
// can be exercised offline. It proves nothing about LUNA, OpenCode or Claude.
//
//   node scripts/qa-opencode-fake-engine.mjs --port 0 [--state <file>]
//        [--drop-reply <intent>] [--lose-command <intent>]
//
// Prints {"port": N} once listening. POST /__fake/restart saves state to
// --state (or a temp file it prints) and exits, so the runner can start a
// new process on that state (a "restarted app" with a new pid).
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import {parseArgs} from 'node:util';

const {values: opt} = parseArgs({options: {
  port: {type: 'string', default: '0'}, state: {type: 'string'},
  'drop-reply': {type: 'string'}, 'lose-command': {type: 'string'}, speed: {type: 'string', default: '1'},
}});
const SPEED = Number(opt.speed);
const tick = ms => new Promise(resolve => setTimeout(resolve, Math.max(1, ms / SPEED)));
const uid = () => crypto.randomUUID();
const nowIso = () => new Date().toISOString();
const NO_EXPIRY = 9223372036854775807; // serialized like i64::MAX (precision is irrelevant here)
const uploadsDir = fs.mkdtempSync(path.join(os.tmpdir(), 'luna-fake-engine-uploads-'));
process.on('exit', () => fs.rmSync(uploadsDir, {recursive: true, force: true}));
const faults = {dropReply: opt['drop-reply'] ?? null, loseCommand: opt['lose-command'] ?? null};

// ------------------------------------------------------------------ state
let state = {deviceId: 'fake-device', spaces: {}, chats: {}, receipts: {}, sideChats: {}, boots: 0};
if (opt.state && fs.existsSync(opt.state)) {
  state = JSON.parse(fs.readFileSync(opt.state, 'utf8'));
  // A restart closes provider-owned asks and running turns, like a server
  // restart; queued rows stay (paused until a new turn ends).
  for (const chat of Object.values(state.chats)) {
    for (const entry of chat.entries) {
      if (entry.status === 'streaming') entry.status = 'aborted';
      for (const part of entry.parts) if (part.kind === 'input') part.resolved = true;
    }
    chat.status = 'idle';
    chat.activeTurnId = null;
  }
}
state.boots += 1;
const live = new Map(); // chatId -> {pending: Map, running: Set, interrupt}
const runtime = id => {
  if (!live.has(id)) live.set(id, {pending: new Map(), interrupted: false, children: []});
  return live.get(id);
};

const subs = {doc: new Set(), sessions: new Set(), queue: new Set(), tasks: new Set(), receipts: new Set()};

function newChat(id, fields) {
  state.chats[id] = {id, config: null, cwd: null, spaceId: null, title: null, harnessSessionId: null, entries: [], revision: 1, sequence: 0,
    bindingGeneration: null, bindingId: null, activeTurnId: null, status: 'idle', lastCompletedTurn: null, queue: [], tasks: [],
    contextControl: {}, seed: null, isDoc: false, ...fields};
  return state.chats[id];
}

// ------------------------------------------------------------------ frames
function tryAppend(prev, next) {
  if (prev.parts.length !== next.parts.length || prev.status !== next.status || prev.role !== next.role) return null;
  let append = null;
  for (let i = 0; i < prev.parts.length; i++) {
    const a = prev.parts[i];
    const b = next.parts[i];
    if (JSON.stringify(a) === JSON.stringify(b)) continue;
    if (!['text', 'reasoning'].includes(a.kind) || a.kind !== b.kind || a.id !== b.id || !b.text.startsWith(a.text) || append) return null;
    append = {entry: next.id, part: b.id, text: b.text.slice(a.text.length), len: Buffer.byteLength(b.text)};
  }
  return append;
}

function diffFrame(prev, next) {
  if (!prev) return {reset: next};
  const nextIds = new Set(next.map(e => e.id));
  const remove = prev.filter(e => !nextIds.has(e.id)).map(e => e.id);
  const prevById = new Map(prev.map((e, i) => [e.id, {e, after: i ? prev[i - 1].id : null}]));
  const upsert = [];
  const append = [];
  next.forEach((entry, i) => {
    const after = i ? next[i - 1].id : null;
    const old = prevById.get(entry.id);
    if (old && old.after === after && JSON.stringify(old.e) === JSON.stringify(entry)) return;
    if (old && old.after === after) {
      const text = tryAppend(old.e, entry);
      if (text) { append.push(text); return; }
    }
    upsert.push({after, entry});
  });
  if (upsert.length * 2 >= Math.max(1, next.length) && next.length > 4) return {reset: next};
  return {upsert, append, remove, count: next.length};
}

function pushDoc(chatId) {
  const chat = state.chats[chatId];
  for (const sub of subs.doc) {
    if (sub.chatId !== chatId) continue;
    const snapshot = structuredClone(chat.entries);
    const frame = diffFrame(sub.last, snapshot);
    sub.last = snapshot;
    sub.send({...frame, contextUsage: chat.entries.length ? {usedTokens: 100 * chat.entries.length, windowTokens: 200000} : null, contextControl: chat.contextControl});
  }
}

const sessionRows = () => Object.values(state.chats).filter(c => !c.isDoc).map(c => ({chatId: c.id, deviceId: state.deviceId, status: c.status,
  startedAt: null, updatedAt: nowIso(), lastCompletedTurn: c.lastCompletedTurn}));
const pushSessions = () => { for (const sub of subs.sessions) sub.send(sessionRows()); };
const pushQueue = chatId => { for (const sub of subs.queue) if (sub.chatId === chatId) sub.send({items: state.chats[chatId].queue}); };
const pushTasks = chatId => { for (const sub of subs.tasks) if (sub.chatId === chatId) sub.send({tasks: state.chats[chatId].tasks}); };

function setStatus(chat, status) {
  chat.status = status;
  pushSessions();
}

// ------------------------------------------------------------------ receipts
function snapshotOf(chat, taskId) {
  if (!chat) return {taskId, taskRevision: 0, taskSequence: 0};
  return {taskId: chat.id, taskRevision: chat.revision, taskSequence: chat.sequence, ...(chat.bindingId ? {bindingId: chat.bindingId, bindingGeneration: chat.bindingGeneration} : {}),
    ...(chat.activeTurnId ? {activeTurnId: chat.activeTurnId} : {})};
}

function receipt(params, status, reason = null) {
  const chat = state.chats[params.taskId];
  const value = {commandId: params.commandId, taskId: params.taskId, intent: params.intent.kind, payloadHash: 'fake', status,
    snapshot: snapshotOf(chat, params.taskId), actorId: params.actorId, clientId: params.clientId, clientSessionId: params.clientSessionId,
    ...(reason ? {reason} : {}), acceptedAtMs: Date.now()};
  state.receipts[params.commandId] = value;
  for (const sub of subs.receipts) if (sub.taskId === params.taskId) sub.send({kind: 'receipt', receipt: value});
  return value;
}

const REVISION = new Set(['nativeReview', 'rename', 'setModel', 'goal', 'compactContext', 'forkTask', 'editEarlierMessage', 'rewindConversation', 'setConfig', 'setPermission']);

// ------------------------------------------------------------------ agent
const textPart = text => ({kind: 'text', id: 't0', text});
const markerAfter = (text, label) => (text.match(new RegExp(`${label}\\s+(\\S+?)[.\\s]*(?:$|\\n)`, 'i')) ?? [])[1];
const exactly = text => markerAfter(text, 'reply with exactly');

function approvalQuestion(chat, requestId, command) {
  const opencode = chat.config.harness === 'opencode';
  const choices = [{label: 'Deny', scope: 'none', description: 'deny'}, {label: 'Allow once', scope: 'request', description: 'once'}, {label: 'Cancel turn', scope: 'none', description: 'stop'}];
  return {id: `approval:${requestId}`, header: 'Approval', question: `Run ${command}?`,
    options: opencode ? choices.map(c => c.label) : ['Allow once', 'Allow for task', 'Deny', 'Cancel'],
    approval: {providerRequestId: requestId, provider: chat.config.harness, machineName: 'fake', actionKind: 'command', summary: command,
      requestedScope: 'request', expiresAtMs: NO_EXPIRY, supportsTaskAllowance: !opencode,
      ...(opencode ? {details: {command, choices, proposedNetworkPolicyAmendments: []}} : {})}};
}

const decisionLabel = (question, decision) => {
  if (decision?.providerChoice) return question.approval.details.choices[decision.providerChoice.index]?.label ?? 'Deny';
  return ({allowOnce: 'Allow once', allowTask: 'Allow for task', deny: 'Deny', cancel: 'Cancel'})[decision] ?? 'Deny';
};

function pngColor(file) {
  try {
    const bytes = fs.readFileSync(file);
    let offset = 8;
    const idat = [];
    while (offset < bytes.length) {
      const len = bytes.readUInt32BE(offset);
      const type = bytes.subarray(offset + 4, offset + 8).toString('ascii');
      if (type === 'IDAT') idat.push(bytes.subarray(offset + 8, offset + 8 + len));
      offset += 12 + len;
    }
    const raw = zlib.inflateSync(Buffer.concat(idat));
    const [r, g, b] = raw.subarray(1, 4);
    return r > g && r > b ? 'red' : g > b ? 'green' : 'blue';
  } catch {
    return 'unknown';
  }
}

async function waitDecision(chat, entry, question, status, requestId = question.approval?.providerRequestId) {
  const part = {kind: 'input', id: `i${entry.parts.length}`, requestId, questions: [question], resolved: false};
  entry.parts.push(part);
  setStatus(chat, status);
  pushDoc(chat.id);
  const answer = await new Promise(resolve => runtime(chat.id).pending.set(requestId, {resolve, question}));
  part.resolved = true;
  if (!['interrupt', 'closed'].includes(answer)) setStatus(chat, 'working');
  pushDoc(chat.id);
  return answer;
}

async function toolRun(chat, entry, call, output, {delayMs = 40, approval = true, isError = false} = {}) {
  const part = {kind: 'tool', id: `p${entry.parts.length}`, call, resolved: false};
  if (approval && chat.config.permissionMode === 'ask' && call.kind === 'exec') {
    const requestId = `perm_${uid().slice(0, 8)}`;
    const answer = await waitDecision(chat, entry, approvalQuestion(chat, requestId, call.command), 'awaitingApproval');
    if (answer === 'interrupt' || answer === 'closed') return 'interrupted';
    const label = decisionLabel(approvalQuestion(chat, requestId, call.command), answer.decision);
    if (label === 'Cancel turn' || label === 'Cancel') {
      entry.parts.push({...part, resolved: true, isError: true, output: 'rejected'});
      return 'cancelled';
    }
    if (label !== 'Allow once' && label !== 'Allow for task') {
      entry.parts.push({...part, resolved: true, isError: true, output: 'The user denied this request in LUNA.'});
      pushDoc(chat.id);
      return 'denied';
    }
  }
  entry.parts.push(part);
  pushDoc(chat.id);
  await tick(delayMs);
  if (runtime(chat.id).interrupted) return 'interrupted';
  Object.assign(part, {resolved: true, isError, output});
  pushDoc(chat.id);
  return 'ok';
}

async function streamText(chat, entry, text) {
  const part = {kind: 'text', id: `t${entry.parts.length}`, text: ''};
  entry.parts.push(part);
  for (const piece of text.match(/.{1,6}/gs) ?? ['']) {
    part.text += piece;
    pushDoc(chat.id);
    await tick(5);
  }
}

function childDoc(chat, label, brief) {
  const id = `${chat.id}--sub--${uid().slice(0, 6)}`;
  const doc = newChat(id, {isDoc: true, config: chat.config, cwd: chat.cwd});
  doc.entries.push({id: uid(), role: 'user', parts: [textPart(brief)], createdAt: Date.now(), deviceId: state.deviceId, status: 'complete'});
  doc.entries.push({id: uid(), role: 'assistant', parts: [], createdAt: Date.now(), deviceId: state.deviceId, status: 'streaming'});
  return doc;
}

async function script(chat, entry, prompt, attachments) {
  const rt = runtime(chat.id);
  const command = (prompt.match(/run exactly this command: ([^\n]+)/) ?? prompt.match(/run this command in the background: ([^\n]+)/) ?? [])[1]?.trim();
  if (/subagent tool/i.test(prompt)) {
    const slow = /sleep 45/.test(prompt);
    const luna = /get_workspace/.test(prompt);
    const briefs = slow ? [['slow', prompt.match(/"([^"]+)"/)?.[1]]] : luna ? [['luna', 'call luna get_workspace']] : [['A', prompt.match(/"(echo CHILD-A-[^"]+)"/)?.[1]], ['B', prompt.match(/"(echo CHILD-B-[^"]+)"/)?.[1]]];
    const chips = [];
    for (const [label, brief] of briefs) {
      const doc = childDoc(chat, label, brief ?? label);
      const chip = {kind: 'tool', id: `p${entry.parts.length}`, call: {kind: 'unknown', name: `Agent: child ${label}`}, resolved: false, subagentRef: doc.id, subagentStatus: 'running'};
      entry.parts.push(chip);
      chips.push({chip, doc, label, brief});
    }
    pushDoc(chat.id);
    const finish = async ({chip, doc, label, brief}) => {
      const assistant = doc.entries.at(-1);
      if (label === 'luna') {
        assistant.parts.push({kind: 'tool', id: 'p0', call: {kind: 'mcp', server: 'luna', tool: 'get_workspace', input: {}}, resolved: true, isError: true, output: 'LUNA tools are not available to subagents.'});
        assistant.parts.push(textPart('The luna call was refused.'));
      } else {
        const cmd = brief.replace(/^sleep \d+;\s*/, '');
        assistant.parts.push({kind: 'tool', id: 'p0', call: {kind: 'exec', command: brief}, resolved: false});
        pushDoc(doc.id);
        await tick(label === 'slow' ? 4000 : label === 'B' ? 300 : 60);
        if (chip.subagentStatus === 'interrupted') return;
        Object.assign(assistant.parts[0], {resolved: true, output: `${cmd.replace(/^echo /, '')}\n`});
        assistant.parts.push(textPart(cmd.replace(/^echo /, '')));
      }
      assistant.status = 'complete';
      Object.assign(chip, {resolved: true, subagentStatus: 'done'});
      pushDoc(doc.id);
      pushDoc(chat.id);
    };
    const foreground = chips.filter(c => c.label !== 'B');
    const background = chips.filter(c => c.label === 'B');
    rt.children = chips;
    if (foreground.some(c => c.label === 'slow')) {
      await Promise.race([finish(foreground[0]), new Promise(resolve => { rt.onInterrupt = resolve; })]);
      if (rt.interrupted) {
        for (const c of chips) if (c.chip.subagentStatus === 'running') Object.assign(c.chip, {subagentStatus: 'interrupted', resolved: true});
        return 'interrupted';
      }
    } else {
      await Promise.all(foreground.map(finish));
    }
    if (background.length) {
      chat.afterTurn = async () => {
        await Promise.all(background.map(finish));
        // The background completion wakes the root: a provider-owned turn.
        const wake = {id: `wake-${uid().slice(0, 8)}`, role: 'assistant', parts: [], createdAt: Date.now(), deviceId: state.deviceId, status: 'streaming'};
        chat.entries.push(wake);
        setStatus(chat, 'working');
        await streamText(chat, wake, 'The background agent finished.');
        wake.status = 'complete';
        chat.lastCompletedTurn = `turn-${uid().slice(0, 8)}`;
        setStatus(chat, 'idle');
        pushDoc(chat.id);
      };
    }
    await streamText(chat, entry, slow ? 'stopped' : luna ? 'The subagent reported that the luna call was refused.' : exactly(prompt) ?? 'done');
    return 'ok';
  }
  if (command) {
    const background = /background/i.test(prompt);
    const sleep = Number(command.match(/^sleep (\d+)/)?.[1] ?? 0);
    const result = await toolRun(chat, entry, {kind: 'exec', command}, `${command.replace(/^sleep \d+;?\s*/, '').replace(/^echo /, '')}\n`, {delayMs: background ? 30 : sleep ? sleep * 120 : 40});
    if (result === 'interrupted' || result === 'cancelled') return result === 'cancelled' ? 'cancelled' : 'interrupted';
    if (background) {
      const task = {taskId: `bg-${uid().slice(0, 6)}`, kind: 'shell', description: command, status: 'running'};
      chat.tasks.push(task);
      pushTasks(chat.id);
      setTimeout(() => { task.status = 'completed'; pushTasks(chat.id); }, 600 / SPEED);
    }
    const reply = result === 'denied' ? (prompt.match(/reply with exactly (S1-DENIED-\S+?)\./)?.[1] ?? 'denied') : exactly(prompt) ?? command.replace(/^echo /, '');
    await streamText(chat, entry, reply);
    return 'ok';
  }
  if (/multiple-choice question/i.test(prompt)) {
    const options = (prompt.match(/options: ([a-z, ]+)\./i)?.[1] ?? 'yes, no').split(',').map(x => x.trim());
    const multi = /multiple selections/i.test(prompt);
    const requestId = `form_${uid().slice(0, 8)}`;
    const answer = await waitDecision(chat, entry, {id: `form:${requestId}:choice`, header: 'Question', question: 'Choose', options, multiSelect: multi}, 'awaitingInput', requestId);
    if (answer === 'interrupt' || answer === 'closed') return 'interrupted';
    const labels = answer.answers?.[0]?.labels ?? [];
    const nonce = prompt.match(/COLOR-(\S+?)-</)?.[1] ?? prompt.match(/PICKED-(\S+?)-/)?.[1];
    await streamText(chat, entry, multi ? `PICKED-${nonce}-${labels.join('+')}` : `COLOR-${nonce}-${labels[0] ?? 'none'}`);
    return 'ok';
  }
  if (/get_workspace/.test(prompt) || /luna MCP tools/.test(prompt)) {
    const cross = /different LUNA project/.test(prompt);
    entry.parts.push({kind: 'tool', id: `p${entry.parts.length}`, call: {kind: 'mcp', server: 'luna', tool: cross ? 'get_workspace' : 'get_workspace', input: cross ? {project: 'other'} : {}},
      resolved: true, isError: cross, output: cross ? 'refused: another project' : '{"project":"fixture"}'});
    pushDoc(chat.id);
    const nonce = prompt.match(/WS-(\S+?)-/)?.[1];
    await streamText(chat, entry, cross ? 'The cross-project request was refused.' : `WS-${nonce}-fixture`);
    return 'ok';
  }
  if (/dominant color/i.test(prompt)) {
    await streamText(chat, entry, attachments.length ? pngColor(attachments[0]) : 'none');
    return 'ok';
  }
  if (prompt.startsWith('/qa-echo')) {
    const file = ['.claude/commands/qa-echo.md', '.opencode/command/qa-echo.md'].map(f => path.join(chat.cwd, f)).find(f => fs.existsSync(f));
    await streamText(chat, entry, file ? fs.readFileSync(file, 'utf8').match(/CMD-ECHO-\S+/)[0] : 'unknown command');
    return 'ok';
  }
  if (/List every marker/i.test(prompt)) {
    const markers = chat.entries.filter(e => e.role === 'user').flatMap(e => [...e.parts.map(p => p.text ?? '').join(' ').matchAll(/Remember this marker: (\S+?)\./g)].map(m => m[1]));
    await streamText(chat, entry, markers.join(' '));
    return 'ok';
  }
  if (/second message/i.test(prompt)) {
    const second = chat.entries.filter(e => e.role === 'user')[1];
    await streamText(chat, entry, (second?.parts?.[0]?.text ?? '').match(/marker: (\S+?)\./)?.[1] ?? 'none');
    return 'ok';
  }
  if (/What text did I select/i.test(prompt)) {
    await streamText(chat, entry, chat.seed ?? 'none');
    return 'ok';
  }
  if (/^Review /.test(prompt)) {
    await streamText(chat, entry, 'Fake review: calc.js add() subtracts.');
    return 'ok';
  }
  await streamText(chat, entry, exactly(prompt) ?? 'ok');
  return 'ok';
}

async function runTurn(chat, {messageId, text, attachments = [], userEntry = true}) {
  const rt = runtime(chat.id);
  rt.interrupted = false;
  const turnId = `turn-${uid().slice(0, 8)}`;
  chat.activeTurnId = turnId;
  if (!chat.harnessSessionId) {
    chat.harnessSessionId = `ses_${uid().slice(0, 12)}`;
    chat.bindingId = `binding-${uid().slice(0, 6)}`;
    chat.bindingGeneration = 1;
  }
  if (userEntry) {
    const parts = [textPart(text), ...attachments.map((file, i) => ({kind: 'image', id: `img${i}`, path: file, name: path.basename(file), mimeType: 'image/png'}))];
    chat.entries.push({id: messageId, role: 'user', parts, createdAt: Date.now(), deviceId: state.deviceId, status: 'complete'});
  }
  const entry = {id: `a-${uid().slice(0, 8)}`, role: 'assistant', parts: [], createdAt: Date.now(), deviceId: state.deviceId, status: 'streaming'};
  chat.entries.push(entry);
  setStatus(chat, 'working');
  pushDoc(chat.id);
  await tick(20);
  let outcome;
  try {
    outcome = await script(chat, entry, text, attachments);
  } catch (error) {
    entry.parts.push({kind: 'error', id: 'e0', message: String(error)});
    outcome = 'error';
  }
  if (rt.interrupted) outcome = 'interrupted';
  entry.status = outcome === 'ok' ? 'complete' : 'aborted';
  chat.activeTurnId = null;
  if (outcome === 'ok') chat.lastCompletedTurn = turnId;
  setStatus(chat, outcome === 'error' ? 'errored' : 'idle');
  pushDoc(chat.id);
  const after = chat.afterTurn;
  chat.afterTurn = null;
  if (after) await after();
  const next = chat.queue.shift();
  if (next) {
    pushQueue(chat.id);
    runTurn(chat, {messageId: next.id, text: next.text, attachments: next.attachments});
  }
}

// ------------------------------------------------------------------ intents
function execute(params) {
  const {taskId, intent} = params;
  const chat = state.chats[taskId];
  if (state.receipts[params.commandId]) return state.receipts[params.commandId];
  if (chat && params.expectedTaskRevision != null && params.expectedTaskRevision !== chat.revision) {
    return receipt(params, 'conflict', {code: 'staleTaskRevision', message: 'This task changed while the action was being applied.'});
  }
  if (chat && params.expectedBindingGeneration != null && params.expectedBindingGeneration !== chat.bindingGeneration) {
    return receipt(params, 'conflict', {code: 'staleBindingGeneration', message: 'The provider session changed.'});
  }
  if (chat && params.expectedTurnId && params.expectedTurnId !== chat.activeTurnId) {
    return receipt(params, 'conflict', {code: 'staleTurn', message: 'The active run changed.'});
  }
  if (REVISION.has(intent.kind) && chat) chat.revision += 1;
  if (chat) chat.sequence += 1;
  switch (intent.kind) {
    case 'createChat': {
      if (chat) return receipt(params, 'rejected', {code: 'commandIdConflict', message: 'exists'});
      newChat(taskId, {config: intent.config, cwd: intent.cwd, spaceId: intent.spaceId});
      pushSessions();
      return receipt(params, 'applied');
    }
    case 'session': {
      const command = intent.command;
      const rt = runtime(taskId);
      if (command.kind === 'run') {
        const value = receipt(params, 'pending');
        setTimeout(() => {
          receipt(params, 'applied');
          runTurn(chat, {messageId: command.messageId, text: command.request.prompt, attachments: command.request.attachments ?? []});
        }, 30 / SPEED);
        return value;
      }
      if (command.kind === 'interrupt') {
        rt.interrupted = true;
        for (const [id, pending] of rt.pending) { pending.resolve('interrupt'); rt.pending.delete(id); }
        rt.onInterrupt?.();
        return receipt(params, 'applied');
      }
      if (command.kind === 'respondApproval' || command.kind === 'respondInput') {
        const pending = rt.pending.get(command.requestId);
        if (!pending) return receipt(params, 'rejected', {code: 'staleTurn', message: 'The request is no longer pending.'});
        rt.pending.delete(command.requestId);
        pending.resolve(command);
        return receipt(params, 'applied');
      }
      return receipt(params, 'rejected', {code: 'unsupported', message: `fake engine: ${command.kind}`});
    }
    case 'queueMessage': {
      chat.queue.push({id: intent.messageId, text: intent.text, attachments: intent.attachments ?? [], revision: 1});
      pushQueue(taskId);
      if (chat.status === 'idle') {
        const next = chat.queue.shift();
        pushQueue(taskId);
        runTurn(chat, {messageId: next.id, text: next.text, attachments: next.attachments});
      }
      return receipt(params, 'applied');
    }
    case 'steerQueuedNow': {
      const index = chat.queue.findIndex(item => item.id === intent.messageId);
      if (index < 0) return receipt(params, 'rejected', {code: 'queueMissing', message: 'gone'});
      const [item] = chat.queue.splice(index, 1);
      chat.queue.unshift(item); // delivered right after the current step
      pushQueue(taskId);
      return receipt(params, 'applied');
    }
    case 'forkTask': {
      if (intent.review.providerSessionId !== chat.harnessSessionId) return receipt(params, 'conflict', {code: 'staleBindingGeneration', message: 'session'});
      let entries = chat.entries;
      if (intent.boundary === 'beforeUserMessage') entries = entries.slice(0, entries.findIndex(e => e.id === intent.messageId));
      if (intent.boundary === 'afterCompletedTurn') entries = entries.slice(0, entries.findIndex(e => e.id === intent.messageId) + 1);
      newChat(intent.newTaskId, {config: intent.config, cwd: chat.cwd, spaceId: chat.spaceId, entries: structuredClone(entries),
        harnessSessionId: `ses_${uid().slice(0, 12)}`, bindingId: `binding-${uid().slice(0, 6)}`, bindingGeneration: 1});
      pushSessions();
      return receipt(params, 'applied');
    }
    case 'editEarlierMessage': {
      const at = chat.entries.findIndex(e => e.id === intent.messageId);
      if (at < 0 || intent.review.messageText !== chat.entries[at].parts.filter(p => p.kind === 'text').map(p => p.text.trim()).filter(Boolean).join('\n')) {
        return receipt(params, 'conflict', {code: 'staleTaskRevision', message: 'reviewed text mismatch'});
      }
      chat.entries = chat.entries.slice(0, at);
      pushDoc(taskId);
      setTimeout(() => runTurn(chat, {messageId: `edit-${uid()}`, text: intent.text}), 20 / SPEED);
      return receipt(params, 'applied');
    }
    case 'rewindConversation': {
      const at = chat.entries.findIndex(e => e.id === intent.messageId);
      if (at < 0) return receipt(params, 'rejected', {code: 'staleTaskRevision', message: 'unknown message'});
      chat.entries = chat.entries.slice(0, at);
      pushDoc(taskId);
      return receipt(params, 'applied');
    }
    case 'nativeReview': {
      setTimeout(() => runTurn(chat, {messageId: intent.messageId, text: intent.request.prompt}), 20 / SPEED);
      return receipt(params, 'applied');
    }
    case 'compactContext': {
      chat.contextControl = {compaction: {state: 'completed', at: Date.now()}};
      pushDoc(taskId);
      return receipt(params, 'applied');
    }
    case 'setModel': {
      chat.config = intent.config;
      return receipt(params, 'applied');
    }
    case 'goal': {
      return chat.config.harness === 'opencode'
        ? receipt(params, 'rejected', {code: 'unsupported', message: 'Goals are unavailable for this provider.'})
        : receipt(params, 'applied');
    }
    default:
      return receipt(params, 'rejected', {code: 'unsupported', message: `fake engine does not implement ${intent.kind}`});
  }
}

// ------------------------------------------------------------------ RPC
const models = {
  opencode: [{id: 'opencode/big-pickle', label: 'Big Pickle', reasoningLevels: ['low', 'high'], options: []}, {id: 'luna-mock/mock-model', label: 'Mock', reasoningLevels: ['low', 'high'], options: []}],
  'claude-code': [{id: 'claude-fake-default', label: 'Claude (fake)', reasoningLevels: ['low', 'medium', 'high'], options: [], isDefault: true}],
};
const uploads = new Map();

function handle(method, params, stream) {
  switch (method) {
    case 'EngineInfo': return {deviceId: state.deviceId, version: 'fake', capabilities: ['side-chat-persistence-v1'], boot: state.boots};
    case 'ListModels': return models[params.harness] ?? [];
    case 'Mutate': if (params.op === 'createSpace') state.spaces[params.spaceId] = params; return {receipt: null};
    case 'GetTaskCommandSnapshot': return {snapshot: snapshotOf(state.chats[params.taskId], params.taskId), events: [], reloadRequired: false};
    case 'GetTaskCommandReceipt': return state.receipts[params.commandId] ?? null;
    case 'ExecuteTaskCommand': return execute(params);
    case 'GetTaskIdentity': {
      const chat = state.chats[params.taskId];
      return {taskId: params.taskId, currentBinding: chat?.bindingId ? {bindingId: chat.bindingId, providerId: chat.config.harness, providerTaskId: chat.harnessSessionId, generation: chat.bindingGeneration} : null};
    }
    case 'GetTaskWork': return {items: []};
    case 'WatchChats': return Object.values(state.chats).filter(c => !c.isDoc).map(c => ({id: c.id, config: c.config, cwd: c.cwd, spaceId: c.spaceId,
      title: c.lastCompletedTurn ? 'Fake generated title' : null, harnessSessionId: c.harnessSessionId}));
    case 'ListCommands': {
      const chat = state.chats[params.chatId];
      return fs.existsSync(path.join(chat?.cwd ?? '/nonexistent', '.claude/commands/qa-echo.md')) ? [{name: 'qa-echo', description: 'LUNA QA echo command'}] : [];
    }
    case 'ListComposerCatalog': return {items: []};
    case 'GetUsageSnapshot': return {providers: []};
    case 'UploadChunk': {
      const chunks = uploads.get(params.uploadId) ?? [];
      chunks[params.seq ?? chunks.length] = Buffer.from(params.data, 'base64');
      uploads.set(params.uploadId, chunks);
      return {ok: true};
    }
    case 'UploadCommit': {
      const file = path.join(uploadsDir, params.uploadId, params.fileName);
      fs.mkdirSync(path.dirname(file), {recursive: true});
      fs.writeFileSync(file, Buffer.concat(uploads.get(params.uploadId) ?? []));
      return {path: file};
    }
    case 'CreateSideChat': {
      const parent = state.chats[params.parentId];
      const id = params.requestId;
      const native = !params.forceSnapshot;
      const at = parent.entries.findIndex(e => e.id === params.originMessageId);
      newChat(id, {config: parent.config, cwd: parent.cwd, spaceId: parent.spaceId, entries: native ? structuredClone(parent.entries.slice(0, at < 0 ? undefined : at + 1)) : [],
        seed: native ? null : params.anchorText, harnessSessionId: native ? `ses_${uid().slice(0, 12)}` : null, bindingId: native ? `binding-${uid().slice(0, 6)}` : null, bindingGeneration: native ? 1 : null});
      const meta = {id, parentId: params.parentId, state: 'active', contextMode: native ? 'nativeFork' : 'snapshot', expiresAtMs: 0};
      state.sideChats[id] = meta;
      pushSessions();
      return {meta, lease: null};
    }
    case 'SendSideChat': {
      const chat = state.chats[params.id];
      if (state.sideChats[params.id]?.state !== 'active') throw new Error('Side chat ended');
      setTimeout(() => runTurn(chat, {messageId: params.messageId, text: params.text}), 20 / SPEED);
      return {accepted: true};
    }
    case 'ReadSideChat': {
      const chat = state.chats[params.id];
      return {meta: state.sideChats[params.id], entries: chat.entries, working: chat.status !== 'idle', before: null};
    }
    case 'WatchDocMessages': {
      if (!state.chats[params.chatId]) newChat(params.chatId, {isDoc: true});
      const sub = {chatId: params.chatId, last: null, send: stream};
      subs.doc.add(sub);
      setImmediate(() => pushDoc(params.chatId));
      return {stream: () => subs.doc.delete(sub)};
    }
    case 'WatchSessions': {
      const sub = {send: stream};
      subs.sessions.add(sub);
      setImmediate(() => stream(sessionRows()));
      return {stream: () => subs.sessions.delete(sub)};
    }
    case 'WatchQueue': {
      const sub = {chatId: params.chatId, send: stream};
      subs.queue.add(sub);
      setImmediate(() => stream({items: state.chats[params.chatId]?.queue ?? []}));
      return {stream: () => subs.queue.delete(sub)};
    }
    case 'WatchNativeTasks': {
      const sub = {chatId: params.chatId, send: stream};
      subs.tasks.add(sub);
      setImmediate(() => stream({tasks: state.chats[params.chatId]?.tasks ?? []}));
      return {stream: () => subs.tasks.delete(sub)};
    }
    case 'WatchTaskCommands': {
      const sub = {taskId: params.taskId, send: stream};
      subs.receipts.add(sub);
      setImmediate(() => stream({kind: 'snapshot', read: {snapshot: snapshotOf(state.chats[params.taskId], params.taskId), events: []}}));
      return {stream: () => subs.receipts.delete(sub)};
    }
    default:
      throw new Error(`fake engine: unknown method ${method}`);
  }
}

// ------------------------------------------------------------------ websocket
function encode(text) {
  const payload = Buffer.from(text);
  const header = payload.length < 126 ? Buffer.from([0x81, payload.length])
    : payload.length < 65536 ? Buffer.from([0x81, 126, payload.length >> 8, payload.length & 0xff])
      : (() => { const h = Buffer.alloc(10); h[0] = 0x81; h[1] = 127; h.writeBigUInt64BE(BigInt(payload.length), 2); return h; })();
  return Buffer.concat([header, payload]);
}

const sockets = new Set();
const server = http.createServer((req, res) => {
  if (req.method === 'POST' && req.url === '/__fake/restart') {
    const file = opt.state ?? path.join(os.tmpdir(), `luna-fake-engine-state-${process.pid}.json`);
    fs.writeFileSync(file, JSON.stringify(state));
    res.writeHead(200, {'content-type': 'application/json'});
    res.end(JSON.stringify({state: file}));
    for (const socket of sockets) socket.destroy();
    setTimeout(() => process.exit(0), 50);
    return;
  }
  res.writeHead(426);
  res.end();
});

server.on('upgrade', (req, socket) => {
  const accept = crypto.createHash('sha1').update(`${req.headers['sec-websocket-key']}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
  socket.write(['HTTP/1.1 101 Switching Protocols', 'Upgrade: websocket', 'Connection: Upgrade', `Sec-WebSocket-Accept: ${accept}`, '', ''].join('\r\n'));
  sockets.add(socket);
  const streams = new Map();
  const send = value => { if (!socket.destroyed) socket.write(encode(JSON.stringify(value))); };
  let buffer = Buffer.alloc(0);
  const onMessage = text => {
    let frame;
    try { frame = JSON.parse(text); } catch { return; }
    if (frame.cancel) { streams.get(frame.id)?.(); streams.delete(frame.id); return; }
    const {id, method, params = {}} = frame;
    if (method === 'ExecuteTaskCommand' && faults.loseCommand && params.intent?.kind === faults.loseCommand) {
      faults.loseCommand = null; // swallowed: never processed, never answered
      return;
    }
    let result;
    try {
      result = handle(method, params, item => send({id, item}));
    } catch (error) {
      send({id, err: {message: String(error.message ?? error)}});
      return;
    }
    if (result && typeof result.stream === 'function') { streams.set(id, result.stream); return; }
    if (method === 'ExecuteTaskCommand' && faults.dropReply && params.intent?.kind === faults.dropReply) {
      faults.dropReply = null; // processed, reply lost: readback must find the receipt
      return;
    }
    send({id, ok: result});
  };
  socket.on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      if (buffer.length < 2) return;
      const opcode = buffer[0] & 0x0f;
      let length = buffer[1] & 0x7f;
      let offset = 2;
      if (length === 126) { if (buffer.length < 4) return; length = buffer.readUInt16BE(2); offset = 4; }
      else if (length === 127) { if (buffer.length < 10) return; length = Number(buffer.readBigUInt64BE(2)); offset = 10; }
      const masked = buffer[1] & 0x80;
      const maskOffset = offset;
      if (masked) offset += 4;
      if (buffer.length < offset + length) return;
      const payload = Buffer.from(buffer.subarray(offset, offset + length));
      if (masked) for (let i = 0; i < payload.length; i++) payload[i] ^= buffer[maskOffset + (i % 4)];
      buffer = buffer.subarray(offset + length);
      if (opcode === 0x8) { socket.end(); return; }
      if (opcode === 0x9) { socket.write(Buffer.concat([Buffer.from([0x8a, payload.length]), payload])); continue; }
      if (opcode === 0x1) for (const line of payload.toString('utf8').split('\n').filter(Boolean)) onMessage(line);
    }
  });
  const close = () => { for (const cancel of streams.values()) cancel(); streams.clear(); sockets.delete(socket); };
  socket.on('close', close);
  socket.on('error', close);
});

server.listen(Number(opt.port), '127.0.0.1', () => {
  console.log(JSON.stringify({port: server.address().port, pid: process.pid, boots: state.boots}));
});
process.on('SIGTERM', () => process.exit(0));
