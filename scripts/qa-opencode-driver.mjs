// The UI's actions over one engine connection, for the OpenCode in-app QA
// drivers. Every write is a recorded send (manifest first, QF-10/QF-17);
// waits ride persistent subscriptions (QF-15). Contract:
// docs/reference/qa-infrastructure.md, "OpenCode QA".
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import path from 'node:path';
import {
  NotAnswerable, ReceiptWatch, SessionWatch, StreamWatch, TranscriptMirror, UnknownOutcome,
  approvalDecision, approvalShape, buildAnswers, captureWindow, isLunaCrossTask, maskNonce,
  normalizeEntries, now, questionShape, recordSend, reviewedText, settleSend, sha256,
  summarizeReceipt, taskCommandParams, terminalReceipt, toolMultiset, waitOn,
} from './qa-opencode-lib.mjs';

const ACTIVE = new Set(['working', 'awaitingApproval', 'awaitingInput']);

// ReviewTarget::label (crates/proto/src/agent.rs).
export const reviewLabel = target => ({
  uncommittedChanges: () => 'Review working changes',
  baseBranch: () => `Review changes against ${target.branch}`,
  commit: () => `Review commit ${target.sha}${target.title ? `: ${target.title}` : ''}`,
  custom: () => `Review with custom instructions:\n\n${target.instructions}`,
})[target.type]();

// history_fork_initial_permission (crates/ui/src/shell.rs).
export const forkInitialPermission = config => (config.effectivePermissionMode === 'full'
  ? (config.permissionMode === 'full' ? 'full' : 'auto')
  : config.effectivePermissionMode === 'auto' ? 'auto' : 'ask');

const errorText = error => String(error?.message ?? error);

export class Driver {
  constructor({rpc, evidence, identity, provider, nonce, rpcTimeoutMs = 45000, ax = false, captureHoldMs = 0}) {
    Object.assign(this, {rpc, evidence, identity, provider, nonce, rpcTimeoutMs, ax, captureHoldMs});
    this.chats = new Map();
    this.receiptWatches = new Map();
    this.steps = [];
    this.step = null;
    this.clientSessionId = crypto.randomUUID();
  }

  async init() {
    this.engine = await this.rpc.call('EngineInfo');
    this.actorId = this.engine.deviceId;
    this.sessions = new SessionWatch(this.rpc, row => this.evidence.append('normalized/session-frames.jsonl', row));
    return this.engine;
  }

  observe(key, value) {
    if (this.step) (this.step.observations[key] ??= []).push(value);
  }

  check(criterion, expected, actual, passed, extra = {}) {
    return this.evidence.check(criterion, expected, actual, passed, {...extra, ...(this.step ? {step: this.step.label} : {})});
  }

  // Reported, never decisive: model prose and optional native behaviour.
  info(criterion, expected, actual, passed) {
    return this.check(criterion, expected, actual, passed, {informational: true});
  }

  // A named step is the parity-diff unit (QF-18/QF-20).
  async runStep(label, fn) {
    const step = {label, startedAt: now(), observations: {}, marks: new Map()};
    for (const chatId of this.chats.keys()) step.marks.set(chatId, this.sessions.mark(chatId));
    this.step = step;
    console.error(`--- step: ${label}`);
    try {
      const value = await fn();
      step.ok = true;
      return value;
    } catch (error) {
      step.ok = false;
      step.error = errorText(error);
      throw error;
    } finally {
      step.endedAt = now();
      step.observations.chats = {};
      for (const [chatId, chat] of this.chats) {
        const mark = step.marks.get(chatId) ?? {seq: 0, lastCompletedTurn: null};
        step.observations.chats[chat.role] = {
          layout: normalizeEntries(chat.mirror.entries),
          tools: toolMultiset(chat.mirror.entries),
          statuses: [...new Set(this.sessions.statusesSince(chatId, mark))],
          pendingInputs: chat.mirror.pendingInputs().length,
          queueMax: Math.max(0, ...chat.queue.frames.filter(f => f.at >= step.startedAt).map(f => (f.item?.items ?? []).length)),
          nativeTasks: nativeTaskKinds(chat.nativeTasks.frames),
          contextControl: chat.mirror.latest.contextControl ? Object.keys(chat.mirror.latest.contextControl).sort() : null,
        };
      }
      delete step.marks;
      this.steps.push(step);
      this.step = null;
      this.evidence.save(`normalized/steps-${this.evidence.phase ?? 'run'}.json`, this.steps);
    }
  }

  attach(chatId, {role, config, cwd, spaceId}) {
    const chat = {id: chatId, role, config, cwd, spaceId};
    chat.mirror = new TranscriptMirror(this.rpc, chatId, {label: role, record: row => this.evidence.append(`normalized/doc-frames-${role}.jsonl`, row)}).start();
    chat.queue = new StreamWatch(this.rpc, 'WatchQueue', {chatId}, row => this.evidence.append(`normalized/queue-frames-${role}.jsonl`, row));
    chat.nativeTasks = new StreamWatch(this.rpc, 'WatchNativeTasks', {chatId}, row => this.evidence.append(`normalized/native-tasks-${role}.jsonl`, row));
    this.chats.set(chatId, chat);
    this.receipts(chatId);
    this.evidence.manifest.tasks[role] = chatId;
    this.evidence.writeManifest();
    return chat;
  }

  chatByRole(role) {
    return [...this.chats.values()].find(chat => chat.role === role) ?? null;
  }

  receipts(taskId) {
    if (!this.receiptWatches.has(taskId)) {
      this.receiptWatches.set(taskId, new ReceiptWatch(this.rpc, taskId, row => this.evidence.append('normalized/receipts.jsonl', row)));
    }
    return this.receiptWatches.get(taskId);
  }

  async awaitReceipt(taskId, commandId, receipt, timeoutMs) {
    const watch = this.receipts(taskId);
    try {
      return await waitOn([watch], () => terminalReceipt(watch.receipts.get(commandId)) && watch.receipts.get(commandId), timeoutMs, `receipt ${commandId}`);
    } catch (error) {
      if (!error.timeout) throw error;
      // One readback at the deadline covers a lagged broadcast; no loop.
      return (await this.rpc.call('GetTaskCommandReceipt', {commandId}).catch(() => null)) ?? receipt;
    }
  }

  async snapshotOf(taskId, targetDeviceId = null) {
    return (await this.rpc.call('GetTaskCommandSnapshot', targetDeviceId ? {taskId, targetDeviceId} : {taskId})).snapshot;
  }

  // ExecuteTaskCommand with the UI's fences; a lost reply is read back once.
  async execute(taskId, intent, options = {}) {
    const {label = intent.kind, sendKind = 'command', awaitTerminal = true, timeoutMs = 90000, extra = {},
      commandId = crypto.randomUUID(), targetDeviceId = null} = options;
    const snapshot = options.snapshot ?? await this.snapshotOf(taskId, targetDeviceId);
    const params = taskCommandParams({taskId, commandId, intent, snapshot, actorId: this.actorId, clientSessionId: this.clientSessionId});
    if (targetDeviceId) params.targetDeviceId = targetDeviceId;
    this.receipts(taskId);
    const send = recordSend(this.evidence, {kind: sendKind, label, step: this.step?.label ?? null, taskId, commandId,
      intent: intent.kind, control: intent.command?.kind ?? null,
      fences: {revision: params.expectedTaskRevision ?? null, binding: params.expectedBindingGeneration ?? null, turn: params.expectedTurnId ?? null},
      ...extra});
    let receipt;
    try {
      receipt = await this.rpc.call('ExecuteTaskCommand', params, {timeoutMs: this.rpcTimeoutMs});
    } catch (error) {
      const readback = await this.rpc.call('GetTaskCommandReceipt', {commandId}).catch(() => null);
      if (!readback) {
        settleSend(this.evidence, send, {outcome: error.unknown ? 'unknown' : 'rejected', error: errorText(error)});
        this.observe('receipts', {intent: intent.kind, control: intent.command?.kind ?? null, status: error.unknown ? 'unknown' : 'rpc-error', reasonCode: null});
        if (error.unknown) throw new UnknownOutcome(`${intent.kind} ${commandId}: ${error.message}; not replayed (QF-17)`);
        throw error;
      }
      receipt = readback;
      send.readback = true;
    }
    if (receipt?.status === 'pending' && awaitTerminal) receipt = await this.awaitReceipt(taskId, commandId, receipt, timeoutMs);
    settleSend(this.evidence, send, {outcome: receipt?.status ?? 'unknown', receipt: summarizeReceipt(receipt)});
    this.observe('receipts', {intent: intent.kind, control: intent.command?.kind ?? null, status: receipt?.status ?? null, reasonCode: receipt?.reason?.code ?? null});
    return {params, receipt, send};
  }

  async createProject(dir) {
    const spaceId = crypto.randomUUID();
    // The live driver has no nonce; name its project after the scenario
    // instead of "OpenCode QA null".
    const label = this.nonce ?? this.evidence.manifest?.scenario ?? 'live';
    await this.rpc.call('Mutate', {op: 'createSpace', spaceId, deviceId: this.engine.deviceId, path: dir, name: `OpenCode QA ${label}`, gitDetected: true});
    this.evidence.manifest.projectId = spaceId;
    this.evidence.writeManifest();
    return spaceId;
  }

  async createChat({role, spaceId, cwd, config, title}) {
    const chatId = crypto.randomUUID();
    this.evidence.manifest.tasks[role] = chatId;
    const {receipt} = await this.execute(chatId, {kind: 'createChat', spaceId, cwd, config, title}, {label: `createChat ${role}`, sendKind: 'create'});
    if (receipt?.status !== 'applied') throw new Error(`createChat ${role} ${receipt?.status}: ${receipt?.reason?.message ?? ''}`);
    this.attach(chatId, {role, config, cwd, spaceId});
    await this.chats.get(chatId).mirror.ready;
    return chatId;
  }

  busy(chatId) {
    return ACTIVE.has(this.sessions.status(chatId));
  }

  // Idle chats get session.run, busy chats queueMessage with holdForTurnEnd
  // (crates/ui/src/composer.rs); the command id is the message id.
  async send(chatId, composerText, {attachments = [], label = 'message', queue = null, inputItems = [], followUpBehavior = null} = {}) {
    // Composer::skill_wire_text: for Claude a leading `$skill` is sent as
    // `/skill`; the prompt hash keeps the composer text (QF-18).
    const text = this.provider === 'claude-code' ? composerText.replace(/^\$(?=[A-Za-z0-9][\w.-]*(\s|$))/, '/') : composerText;
    const chat = this.chats.get(chatId);
    const mark = this.sessions.mark(chatId);
    const messageId = crypto.randomUUID();
    const useQueue = queue ?? this.busy(chatId);
    const config = chat.config;
    const intent = useQueue
      // The composer's Steer mode queues with followUpBehavior "steer"; the
      // engine then tries a native in-turn steer before holding the row.
      ? {kind: 'queueMessage', messageId, text, attachments, inputItems, holdForTurnEnd: true, execution: config,
        ...(followUpBehavior ? {followUpBehavior} : {})}
      : {kind: 'session', command: {kind: 'run', messageId, request: {
        prompt: text, inputItems, harness: config.harness, providerAccountId: config.providerAccountId ?? null,
        model: config.model ?? null, reasoning: config.reasoning ?? null, modelOptions: config.modelOptions ?? {}, cwd: chat.cwd,
        sandbox: 'workspace-write', autoApprove: false, permissionMode: config.permissionMode, parentPermissionCeiling: null,
        environmentPermissionCeiling: null, permissionPolicy: null, resume: null, attachments}}};
    const path = useQueue ? 'queueMessage' : 'run';
    const {receipt} = await this.execute(chatId, intent, {label, sendKind: 'message', commandId: messageId, awaitTerminal: false,
      extra: {chatRole: chat.role, messageId, path, prompt: text, promptSha256: sha256(maskNonce(composerText, this.nonce)), attachments, inputItems}});
    if (!['applied', 'pending'].includes(receipt?.status)) throw new Error(`send on ${chat.role} ${receipt?.status}: ${receipt?.reason?.message ?? ''}`);
    return {messageId, mark, receipt, path};
  }

  replySettled(entries, messageId) {
    const at = entries.findIndex(e => e.id === messageId);
    return at >= 0 && entries.slice(at + 1).some(e => e.role === 'assistant' && ['complete', 'aborted'].includes(e.status));
  }

  // Settles on frames, tied to our own message when its id is known: the
  // user row is in the transcript, the session is idle/errored with nothing
  // streaming, and either the reply after it settled or the session was seen
  // active after the row appeared. A provider-owned wake turn (background
  // child completion) therefore never settles someone else's turn. Without a
  // message id (edit, review) activity after `mark` is required. A new
  // unresolved input goes to `answer(pending)` or ends the wait; `ignore`
  // lists request ids that must not count (closed by a Stop).
  async waitTurn(chatId, {mark, messageId = null, answer = null, timeoutMs = 240000, ignore = []} = {}) {
    const chat = this.chats.get(chatId);
    const deadline = Date.now() + timeoutMs;
    const answered = new Set(ignore);
    const handled = [];
    let sawActive = false;
    const result = fields => ({status: this.sessions.status(chatId), ...fields, statuses: this.sessions.statusesSince(chatId, mark), handled});
    for (;;) {
      let outcome;
      try {
        outcome = await waitOn([this.sessions, chat.mirror], () => {
          const pending = chat.mirror.currentPending();
          if (pending && !answered.has(pending.requestId)) return {type: 'pending', pending};
          const status = this.sessions.status(chatId);
          const entries = chat.mirror.entries;
          const streaming = entries.some(e => e.status === 'streaming');
          const resting = ['idle', 'errored'].includes(status) && !streaming;
          if (messageId) {
            const userSeen = entries.some(e => e.id === messageId);
            if (userSeen && ACTIVE.has(status)) sawActive = true;
            if (resting && userSeen && (this.replySettled(entries, messageId) || sawActive || status === 'errored')) return {type: 'settled', status};
            // A task with no session row at all settles on the document
            // alone: a settled reply after our row and nothing streaming.
            if (!this.sessions.rows.has(chatId) && !streaming && userSeen && this.replySettled(entries, messageId)) return {type: 'settled', status: 'idle', by: 'document'};
            return null;
          }
          const active = this.sessions.since(chatId, mark).some(h => ACTIVE.has(h.status));
          if (resting && active) return {type: 'settled', status};
          return null;
        }, Math.max(1, deadline - Date.now()), `turn on ${chat.role}`);
      } catch (error) {
        if (!error.timeout) throw error;
        return result({result: 'timeout'});
      }
      if (outcome.type === 'settled') {
        // Session and transcript are separate subscriptions: the session can
        // report idle a few hundred ms before the reply entry lands (seen
        // live). Give the reply a short grace and record the lag.
        if (messageId && outcome.status === 'idle' && !this.replySettled(chat.mirror.entries, messageId)) {
          const idleAt = Date.now();
          const late = await chat.mirror.waitFor(entries => this.replySettled(entries, messageId), {timeoutMs: 5000, label: 'reply after idle'}).then(() => true, () => false);
          this.observe('replyAfterIdle', {chat: chat.role, arrived: late, lagMs: Date.now() - idleAt});
          if (late) return result({result: 'settled', status: outcome.status, settledBy: 'session-then-document', replyLagMs: Date.now() - idleAt});
        }
        return result({result: 'settled', status: outcome.status, settledBy: outcome.by ?? 'session'});
      }
      const {pending} = outcome;
      this.observe('inputs', {chat: chat.role, questions: pending.questions.map(questionShape)});
      const action = answer ? await answer(pending) : null;
      if (!action) return result({result: 'pending', pending});
      answered.add(pending.requestId);
      const {receipt} = await this.answer(chatId, pending, action);
      handled.push({requestId: pending.requestId, action, status: receipt?.status ?? null});
    }
  }

  // A user waits for a running turn (a provider wake, a queued delivery)
  // before typing; returns how long that took (0 when already idle).
  async waitIdle(chatId, {timeoutMs = 120000} = {}) {
    if (!this.busy(chatId)) return 0;
    const started = Date.now();
    await waitOn([this.sessions], () => !this.busy(chatId), timeoutMs, `idle ${this.chats.get(chatId)?.role ?? chatId}`);
    this.observe('waitedForIdle', {chat: this.chats.get(chatId)?.role ?? chatId});
    return Date.now() - started;
  }

  async waitPending(chatId, {timeoutMs = 180000} = {}) {
    const chat = this.chats.get(chatId);
    return chat.mirror.waitFor(() => chat.mirror.currentPending(), {timeoutMs, label: `pending input on ${chat.role}`});
  }

  // QF-12/QF-13: the UI's answer path. `action` is {approval: 'allow-once'|
  // 'allow-task'|'deny'|'cancel'} or a buildAnswers policy.
  async answer(chatId, pending, action, {label = null, probe = false} = {}) {
    const chat = this.chats.get(chatId);
    const questions = pending.questions ?? [];
    if (questions.some(isLunaCrossTask)) throw new NotAnswerable('LUNA cross-task approval: answered only through the in-process UI route (QF-13)');
    const approvalQuestion = questions.find(q => q.approval);
    let command;
    let detail;
    if (approvalQuestion) {
      assert.ok(action.approval, 'an approval needs {approval: allow-once|allow-task|deny|cancel}');
      const {decision, label: choice} = approvalDecision(approvalQuestion, action.approval);
      command = {kind: 'respondApproval', requestId: pending.requestId, decision};
      detail = {choice, decision};
      this.observe('approvals', {chat: chat?.role ?? chatId, action: action.approval, choice, shape: approvalShape(approvalQuestion.approval)});
    } else {
      const answers = buildAnswers(questions, action);
      command = {kind: 'respondInput', requestId: pending.requestId, answers};
      detail = {answers};
      this.observe('questions', {chat: chat?.role ?? chatId, shapes: questions.map(questionShape), answered: answers.map(a => a.labels.length)});
    }
    return this.execute(chatId, {kind: 'session', command}, {label: label ?? (probe ? 'stale-answer probe' : 'answer'), sendKind: probe ? 'probe' : 'answer',
      extra: {chatRole: chat?.role ?? null, requestId: pending.requestId, probe, ...detail}});
  }

  // Answer (or amend) a nonblocking question card the way the panel does:
  // SessionCommandPayload::RespondAsyncQuestion keyed by the card id, with
  // answers keyed by question index (crates/ui/src/composer.rs).
  async answerAsync(chatId, messageId, answers, {label = 'async answer'} = {}) {
    return this.execute(chatId, {kind: 'session', command: {kind: 'respondAsyncQuestion', messageId, answers}},
      {label, sendKind: 'answer', extra: {chatRole: this.chats.get(chatId)?.role ?? null, messageId, answers}});
  }

  // The question panel's "Keep waiting" (any panel interaction sends the
  // same): cancel LUNA's unattended answer for this request
  // (SessionCommandPayload::HoldInput, crates/ui/src/composer.rs).
  async holdInput(chatId, requestId, {label = 'hold input'} = {}) {
    return this.execute(chatId, {kind: 'session', command: {kind: 'holdInput', requestId}},
      {label, sendKind: 'hold', extra: {chatRole: this.chats.get(chatId)?.role ?? null, requestId}});
  }

  async stop(chatId, {label = 'stop'} = {}) {
    return this.execute(chatId, {kind: 'session', command: {kind: 'interrupt'}}, {label, sendKind: 'stop', extra: {chatRole: this.chats.get(chatId)?.role ?? null}});
  }

  async steerQueued(chatId, messageId) {
    const chat = this.chats.get(chatId);
    const row = (chat.queue.latest?.items ?? []).find(item => item.id === messageId);
    return this.execute(chatId, {kind: 'steerQueuedNow', messageId, expectedQueueRevision: row?.revision ?? null},
      {label: 'steer queued now', sendKind: 'steer', extra: {chatRole: chat.role, messageId}});
  }

  async sendQueued(chatId, messageId) {
    const chat = this.chats.get(chatId);
    const row = (chat.queue.latest?.items ?? []).find(item => item.id === messageId);
    return this.execute(chatId, {kind: 'sendQueuedNow', messageId, expectedQueueRevision: row?.revision ?? null},
      {label: 'send queued now', sendKind: 'message', extra: {chatRole: chat.role, messageId}});
  }

  async chatRecord(taskId) {
    const value = await this.rpc.call('WatchChats', {});
    const list = Array.isArray(value) ? value : value?.chats ?? [];
    return list.find(chat => chat.id === taskId) ?? null;
  }

  // QF-16: the UI's HistoryReviewToken (crates/ui/src/shell.rs).
  async reviewToken(taskId, messageId = null) {
    const chat = this.chats.get(taskId);
    const snapshot = await this.snapshotOf(taskId);
    const record = await this.chatRecord(taskId);
    const identity = await this.rpc.call('GetTaskIdentity', {taskId});
    const providerSessionId = record?.harnessSessionId ?? identity?.currentBinding?.providerTaskId ?? null;
    assert.ok(providerSessionId, 'task has no provider history yet');
    assert.ok(snapshot.bindingGeneration != null, 'task binding is not materialized');
    const config = record?.config ?? chat.config;
    const entry = messageId ? chat.mirror.entries.find(e => e.id === messageId) : null;
    if (messageId) assert.ok(entry, `message ${messageId} is not in the mirrored transcript`);
    const review = {taskRevision: snapshot.taskRevision, bindingGeneration: snapshot.bindingGeneration, providerSessionId,
      providerAccountId: config.providerAccountId ?? null, cwd: record?.cwd ?? chat.cwd ?? '.', messageId, messageText: entry ? reviewedText(entry) : null};
    return {snapshot, review, config};
  }

  // The UI's fork lineage link (crates/ui/src/links.rs workspace_locator and
  // luna_conversation_link). Only the local scope is reproducible over RPC:
  // the synced scopes hash the signed-in user, which the engine does not
  // expose, so they get null and the engine refuses the fork truthfully.
  sourceLink(taskId) {
    if (this.engine?.workspaceScope !== 'local' || !this.engine?.deviceId) return null;
    const workspace = crypto.createHash('sha256').update(`Local\0device:${this.engine.deviceId}`).digest('hex').slice(0, 16);
    return `luna://open/chat/${encodeURIComponent(taskId)}?workspace=${encodeURIComponent(workspace)}`;
  }

  async fork(taskId, {boundary, messageId = null, title = null, initialPrompt = null, role}) {
    const {snapshot, review, config} = await this.reviewToken(taskId, messageId);
    const newTaskId = crypto.randomUUID();
    const forkConfig = {...config, permissionMode: forkInitialPermission(config)};
    const result = await this.execute(taskId, {kind: 'forkTask', newTaskId, boundary, messageId, title, initialPrompt, config: forkConfig, review, sourceLink: this.sourceLink(taskId)},
      {label: `fork ${boundary}`, sendKind: 'history', snapshot, extra: {newTaskId, boundary, messageId, role}});
    if (result.receipt?.status === 'applied') {
      const source = this.chats.get(taskId);
      this.attach(newTaskId, {role, config: forkConfig, cwd: source.cwd, spaceId: source.spaceId});
      await this.chats.get(newTaskId).mirror.ready;
    }
    return {...result, newTaskId};
  }

  async editEarlier(taskId, messageId, text) {
    const {snapshot, review} = await this.reviewToken(taskId, messageId);
    const mark = this.sessions.mark(taskId);
    const result = await this.execute(taskId, {kind: 'editEarlierMessage', messageId, text, review}, {label: 'edit earlier message', sendKind: 'history', snapshot,
      extra: {messageId, prompt: text, promptSha256: sha256(maskNonce(text, this.nonce))}});
    return {...result, mark};
  }

  async rewind(taskId, messageId) {
    const {snapshot, review} = await this.reviewToken(taskId, messageId);
    return this.execute(taskId, {kind: 'rewindConversation', messageId, keepFiles: true, review}, {label: 'rewind conversation', sendKind: 'history', snapshot, extra: {messageId}});
  }

  async createSideChat(parentId, {role, snapshot = false, originMessageId = null, anchorText = null}) {
    const requestId = crypto.randomUUID();
    const send = recordSend(this.evidence, {kind: 'sideChatCreate', step: this.step?.label ?? null, parentId, requestId, forceSnapshot: snapshot, originMessageId, anchorText, role});
    let created;
    try {
      created = await this.rpc.call('CreateSideChat', {parentId, requestId, originMessageId, anchorText, forceSnapshot: snapshot}, {timeoutMs: 120000});
    } catch (error) {
      settleSend(this.evidence, send, {outcome: error.unknown ? 'unknown' : 'rejected', error: errorText(error)});
      this.observe('sideChats', {role, outcome: error.unknown ? 'unknown' : 'rejected'});
      if (error.unknown) throw new UnknownOutcome(`CreateSideChat ${requestId}: ${error.message}; not replayed (QF-17)`);
      return {error: errorText(error)};
    }
    settleSend(this.evidence, send, {outcome: 'applied', meta: created?.meta ?? null});
    const id = created?.meta?.id ?? requestId;
    const parent = this.chats.get(parentId);
    this.attach(id, {role, config: parent.config, cwd: parent.cwd, spaceId: parent.spaceId});
    this.observe('sideChats', {role, outcome: 'applied', contextMode: created?.meta?.contextMode ?? null, state: created?.meta?.state ?? null});
    return {id, created};
  }

  async sendSideChat(id, text) {
    const mark = this.sessions.mark(id);
    const messageId = crypto.randomUUID();
    const send = recordSend(this.evidence, {kind: 'sideChatMessage', step: this.step?.label ?? null, sideId: id, messageId, prompt: text, promptSha256: sha256(maskNonce(text, this.nonce))});
    try {
      settleSend(this.evidence, send, {outcome: 'applied', response: await this.rpc.call('SendSideChat', {id, messageId, text})});
    } catch (error) {
      settleSend(this.evidence, send, {outcome: error.unknown ? 'unknown' : 'rejected', error: errorText(error)});
      if (error.unknown) throw new UnknownOutcome(`SendSideChat ${messageId}: ${error.message}; not replayed (QF-17)`);
      throw error;
    }
    return {messageId, mark};
  }

  async review(taskId, target) {
    const chat = this.chats.get(taskId);
    const deviceId = this.engine.deviceId;
    const snapshot = await this.snapshotOf(taskId, deviceId);
    const record = await this.chatRecord(taskId);
    const config = record?.config ?? chat.config;
    const messageId = crypto.randomUUID();
    const request = {review: {target}, prompt: reviewLabel(target), inputItems: [], harness: config.harness, providerAccountId: config.providerAccountId ?? null,
      model: config.model ?? null, reasoning: config.reasoning ?? null, modelOptions: config.modelOptions ?? {}, cwd: record?.cwd ?? chat.cwd,
      sandbox: config.sandbox, autoApprove: false, permissionMode: config.permissionMode, parentPermissionCeiling: null,
      environmentPermissionCeiling: null, permissionPolicy: null, resume: null, attachments: []};
    const mark = this.sessions.mark(taskId);
    const result = await this.execute(taskId, {kind: 'nativeReview', request, messageId}, {label: `review ${target.type}`, sendKind: 'review', snapshot,
      targetDeviceId: deviceId, extra: {messageId, target: target.type}});
    return {...result, messageId, mark};
  }

  async compact(taskId) {
    return this.execute(taskId, {kind: 'compactContext'}, {label: 'compact context', sendKind: 'compact'});
  }

  async setModel(taskId, patch) {
    const chat = this.chats.get(taskId);
    const record = await this.chatRecord(taskId);
    const config = {...(record?.config ?? chat.config), ...patch};
    const result = await this.execute(taskId, {kind: 'setModel', config}, {label: 'set model', sendKind: 'config', extra: {model: config.model, reasoning: config.reasoning ?? null}});
    if (result.receipt?.status === 'applied') chat.config = config;
    return result;
  }

  async goal(taskId, operation) {
    return this.execute(taskId, {kind: 'goal', operation}, {label: 'goal', sendKind: 'goal'});
  }

  async upload(bytes, fileName) {
    const uploadId = crypto.randomUUID();
    const chunk = 48 * 1024;
    for (let offset = 0, seq = 0; offset < bytes.length; offset += chunk, seq++) {
      await this.rpc.call('UploadChunk', {uploadId, data: bytes.subarray(offset, offset + chunk).toString('base64'), seq});
    }
    const {path: staged} = await this.rpc.call('UploadCommit', {uploadId, fileName});
    const row = {uploadId, fileName, bytes: bytes.length, sha256: sha256(bytes), path: staged};
    (this.evidence.manifest.attachments ??= []).push(row);
    this.evidence.writeManifest();
    return row;
  }

  // Milestone readback: single reads plus the mirrored state (no polling).
  async snapshot(label) {
    const state = {label, at: now(), pid: this.identity.pid, chats: {}};
    for (const [chatId, chat] of this.chats) {
      const read = (method, params) => this.rpc.call(method, params).catch(error => ({error: errorText(error)}));
      const [identity, work, commands] = await Promise.all([
        read('GetTaskIdentity', {taskId: chatId}),
        read('GetTaskWork', {taskId: chatId, limit: 100}),
        read('GetTaskCommandSnapshot', {taskId: chatId}),
      ]);
      state.chats[chat.role] = {chatId, identity, work, commands, session: this.sessions.rows.get(chatId) ?? null,
        transcript: chat.mirror.entries, queue: chat.queue.latest, nativeTasks: chat.nativeTasks.latest, latest: chat.mirror.latest};
    }
    this.evidence.save(`state/${label.replace(/[^a-z0-9._-]+/gi, '-')}.json`, state);
    return state;
  }

  // --capture-hold-ms: keep a live card or panel unanswered for that long
  // before the capture, so the operator can bring the chat on screen and
  // record it (presentation-only, QF-19). The hold and its window are logged.
  async holdForCapture(label) {
    if (this.captureHoldMs > 0 && !this.identity.rehearsal) {
      const at = now();
      console.error(`HOLD ${label}: card pending, capturing in ${this.captureHoldMs} ms (chat ${this.step?.label ?? '-'})`);
      this.evidence.save(path.join('holds', `${label.replace(/[^a-z0-9._-]+/gi, '-')}.json`), {label, at, holdMs: this.captureHoldMs, step: this.step?.label ?? null});
      await new Promise(resolve => setTimeout(resolve, this.captureHoldMs));
    }
    return this.capture(label);
  }

  capture(label) {
    if (this.identity.rehearsal) return {label, screenshot: {status: 'skipped', reason: 'rehearsal has no app window'}, ax: null};
    const record = captureWindow(this.evidence, this.identity.pid, label, {ax: this.ax});
    this.observe('captures', {label, screenshot: record.screenshot?.status, ax: record.ax?.status});
    return record;
  }

  // QF-22: one turn-fenced Stop per owned chat still running; never quits.
  async cleanup() {
    const stopped = [];
    for (const [chatId, chat] of this.chats) {
      if (!this.busy(chatId)) continue;
      try {
        stopped.push({role: chat.role, status: (await this.stop(chatId, {label: 'cleanup stop (QF-22)'})).receipt?.status ?? null});
      } catch (error) {
        stopped.push({role: chat.role, error: errorText(error)});
      }
    }
    return stopped;
  }

  close() {
    this.evidence.save('observer.json', {
      policy: 'persistent WatchDocMessages/WatchSessions/WatchQueue/WatchTaskCommands/WatchNativeTasks; snapshots are single reads at milestones (QF-15)',
      subscriptions: this.rpc.subscriptions.map(s => s.record),
      transcripts: [...this.chats.values()].map(c => ({role: c.role, updates: c.mirror.updates, resets: c.mirror.resets, desyncs: c.mirror.desyncs})),
    });
    this.rpc.close();
  }
}

function nativeTaskKinds(frames) {
  const kinds = new Set();
  for (const frame of frames) {
    const item = frame.item;
    const rows = Array.isArray(item) ? item : item?.tasks ?? item?.items ?? [];
    for (const row of rows) {
      const kind = row?.kind ?? row?.task?.kind ?? row?.taskKind;
      if (kind) kinds.add(typeof kind === 'string' ? kind : JSON.stringify(kind));
    }
  }
  return [...kinds].sort();
}
