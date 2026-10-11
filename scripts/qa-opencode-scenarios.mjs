#!/usr/bin/env node
// OpenCode in-app scenario runner: S1..S8 through the signed LUNA QA app's
// RPC surface (the RPCs the UI uses), with a provider argument so the same
// steps run against Claude Code for the PD-14 parity diff.
//
//   node scripts/qa-opencode-scenarios.mjs list
//   node scripts/qa-opencode-scenarios.mjs dry-run <S#|all> [--provider P] [--tier T]
//   node scripts/qa-opencode-scenarios.mjs self-check            (offline, no app)
//   node scripts/qa-opencode-scenarios.mjs rehearse [<S#|all>]   (offline fake engine, tier R0)
//   node scripts/qa-opencode-scenarios.mjs rehearse-faults       (offline: lost reply / unknown outcome, QF-17)
//   node scripts/qa-opencode-scenarios.mjs run <S#> --provider opencode|claude-code
//        --tier T2|T3 [--model <id>] [--variant <level>] [--evidence-root <dir>]
//        [--turn-timeout <s>] [--allow-usage-store] [--ax] [--phase pre|post --run <dir>]
//        [--capture-hold-ms <ms>]  (keep the S1 approval card, S1 question panel
//        and S2X late card unanswered that long so the operator can capture them)
//   S9Q and S9S also accept --provider codex --tier T3 --model <explicit model>.
//   Other scenarios and fake-engine parity remain OpenCode/Claude only.
//   Live-only extras (never rehearsed): S2X background child ask after the
//   root Done, parked follow-up and per-child Stop; S4X OpenCode server
//   SIGSTOP/SIGCONT and SIGKILL with an approval or a form pending
//   [--hold-ms] [--mock-url at T2]; S4S provider stall mid-turn (T2 mock with a long --slow-ms)
//   [--stall-wait-ms] (also checks the engine's Stalled notice; run the mock
//   with --slow-ms above 120000); S9U unattended answers: recommended option
//   auto-picked, Keep waiting, taste questions wait (T3, both providers; set
//   the QA data dir ui-settings.json unattendedAnswerSeconds to match
//   [--unattended-ms], default 15000); S9Q nonblocking questions through
//   LUNA ask_user (T3, both providers): keep working, late answer as a new
//   turn, amendment, idempotent repeat, answer steered into a running turn;
//   S9S live steer: an image steer row joins the running turn, and a steer
//   racing turn completion is delivered once (steered or held, then sent);
//   S8X automatic compaction with a
//   small-context mock model (T2) [--big-command].
//
// Scenarios (validation-plan.md §4): S1 OCE-16/05 approvals and questions;
// S2 OCE-01/21 subagent attribution and background work; S3 OCE-18 LUNA tool
// delivery; S4 OCE-04/05/09 restart with a pending approval and a queued row
// (two phases; the QA operator restarts the app in between); S5 OCE-10/24
// fork, edit, rewind and side chats; S6 OCE-13 models, variants and usage;
// S7 OCE-08/14/20 images, compaction and discovery; S8 OCE-23 Review.
//
// Contract and failure cases QF-09..QF-24: docs/reference/qa-infrastructure.md.
// Identity is asserted before any write; every send is in manifest.json before
// its RPC and an unknown send is never replayed; the runner never builds,
// launches or quits an app.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {parseArgs} from 'node:util';
import {
  Evidence, NotAnswerable, Rpc, UnknownOutcome, acceptsTypedAnswer, applyTranscriptFrame, approvalDecision, assertQaIdentity,
  assertRehearsalIdentity, buildAnswers, captureWireSlice, fileHashes, finishEvidence, isLunaCrossTask, lunaSource,
  makeFixture, maskNonce, normalizeEntries, now, readJsonl, recordSend, root, settleSend, sha256, solidPng, spawnFakeEngine,
  taskCommandParams, textOf, waitOn, wireOffsets,
} from './qa-opencode-lib.mjs';
import {Driver, forkInitialPermission, reviewLabel} from './qa-opencode-driver.mjs';

const PROVIDERS = ['opencode', 'claude-code'];
// Subagent prompts name the agent type: with a bare "subagent tool" request
// gpt-6-luna picked OpenCode's read-only `explore` agent, whose own rules deny
// shell, so the child never asked (e2e-paid-r2 S2X). Both providers have a
// general-purpose type with a shell tool; the text stays provider-neutral.
const SHELL_AGENT = 'Use the general-purpose subagent type (one that has a shell tool), not a read-only explore agent.';
const BLOCKING_QUESTION = "Use your provider-native blocking question tool, not LUNA ask_user. This test requires the current turn to pause until my answer arrives.";

// ------------------------------------------------------------------ context
class ScenarioContext {
  constructor({driver, evidence, provider, tier, nonce, fixture, config, opts}) {
    Object.assign(this, {d: driver, evidence, provider, tier, nonce, fixture, config, opts});
    this.turnTimeout = Number(opts['turn-timeout'] ?? 240) * 1000;
    this.ids = {};
  }

  m(name) { return `${name}-${this.nonce}`; }

  // T2 OpenCode runs append the mock provider's marker; the prompt hash in the
  // manifest masks the nonce, and the parity runs are T3 (QF-18).
  p(text, mock) {
    return this.tier === 'T2' && this.provider === 'opencode' && mock ? `${text} [[mock:${mock}]]` : text;
  }

  chatId(role) {
    const chat = this.d.chatByRole(role);
    assert.ok(chat, `no chat with role ${role}`);
    return chat.id;
  }

  entries(role) { return this.d.chatByRole(role)?.mirror.entries ?? []; }

  after(role, messageId) {
    const entries = this.entries(role);
    const at = entries.findIndex(e => e.id === messageId);
    return at < 0 ? [] : entries.slice(at + 1);
  }

  replyAfter(role, messageId) {
    return this.after(role, messageId).filter(e => e.role === 'assistant').map(textOf).join('\n').trim();
  }

  toolsAfter(role, messageId) {
    return this.after(role, messageId).flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool'));
  }

  users(role) { return this.entries(role).filter(e => e.role === 'user'); }

  step(label, fn) { return this.d.runStep(label, fn); }
  check(...args) { return this.d.check(...args); }
  info(...args) { return this.d.info(...args); }

  // Marker checks are decisive at T3 only: a T2 mock does not follow prose.
  modelCheck(criterion, expected, actual, passed) {
    return this.tier === 'T3' ? this.check(criterion, expected, actual, passed) : this.info(criterion, expected, actual, passed);
  }

  settled(wait, label = 'turn-settled', statuses = ['idle']) {
    return this.check(label, `the turn settles (${statuses.join('/')}) within the turn timeout`, `${wait.result}:${wait.status} via ${wait.statuses.join('>') || '-'}`,
      wait.result === 'settled' && statuses.includes(wait.status));
  }

  async turn(role, text, {mock = null, answer = null, attachments = [], queue = null, timeoutMs = this.turnTimeout, label = 'message', inputItems = []} = {}) {
    const chatId = this.chatId(role);
    if (queue !== true) await this.d.waitIdle(chatId).catch(() => null);
    const sent = await this.d.send(chatId, this.p(text, mock), {attachments, queue, label, inputItems});
    const wait = await this.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, answer, timeoutMs});
    return {...sent, wait, reply: this.replyAfter(role, sent.messageId)};
  }

  async waitSideTurn(role, sent, {answer = null, timeoutMs = this.turnTimeout} = {}) {
    const wait = await this.d.waitTurn(this.chatId(role), {mark: sent.mark, messageId: sent.messageId, answer, timeoutMs});
    return {...sent, wait, reply: this.replyAfter(role, sent.messageId)};
  }

  // Waits on frames (no RPC) for `ms` and reports whether `bad()` became
  // true meanwhile; used to show something does NOT happen.
  async stays(role, bad, ms) {
    const mirror = this.d.chatByRole(role).mirror;
    try {
      await mirror.waitFor(() => bad(mirror.entries), {timeoutMs: ms, label: 'unexpected change'});
      return false;
    } catch (error) {
      if (error.timeout) return true;
      throw error;
    }
  }
}

const markerInTools = (tools, marker) => tools.some(t => !t.isError && String(t.output ?? '').includes(marker));
const allTools = entries => entries.flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool'));
const AUTO_DECLINED = /closed this approval without an answer|dismissed without an answer/i;
const textDump = entries => entries.map(e => JSON.stringify(e.parts ?? [])).join('\n');

// Waits on frames for `done(entries, handled)` while answering each new
// pending request with `policy(pending)` through the UI path; a request the
// policy declines ends the wait with {pending}.
async function waitAnswering(s, role, done, {policy = allowAll, timeoutMs = 180000, label = 'condition'} = {}) {
  const chatId = s.chatId(role);
  const chat = s.d.chatByRole(role);
  const answered = new Set();
  const handled = [];
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    let outcome;
    try {
      outcome = await waitOn([s.d.sessions, chat.mirror], () => {
        const value = done(chat.mirror.entries, handled);
        if (value) return {type: 'done', value};
        const pending = chat.mirror.currentPending();
        if (pending && !answered.has(pending.requestId)) return {type: 'pending', pending};
        return null;
      }, Math.max(1, deadline - Date.now()), label);
    } catch (error) {
      if (error.timeout) return {timeout: true, handled};
      throw error;
    }
    if (outcome.type === 'done') return {value: outcome.value, handled};
    const action = await policy(outcome.pending);
    if (!action) return {pending: outcome.pending, handled};
    answered.add(outcome.pending.requestId);
    const {receipt} = await s.d.answer(chatId, outcome.pending, action);
    handled.push({requestId: outcome.pending.requestId, action, status: receipt?.status ?? null, at: now()});
  }
}

// Operator fault injection (S4X): the exact `opencode serve --stdio` child of
// the verified QA app pid whose cwd is this scenario's fixture. Nothing else
// (the user's own `opencode serve --service`, other chats' servers) matches.
function opencodeServersFor(appPid, dir) {
  const want = fs.realpathSync(dir);
  const rows = spawnSync('ps', ['-axo', 'pid=,ppid=,command='], {encoding: 'utf8'}).stdout.split('\n')
    .map(line => line.trim().match(/^(\d+)\s+(\d+)\s+(.*)$/)).filter(Boolean)
    .map(m => ({pid: Number(m[1]), ppid: Number(m[2]), command: m[3]}))
    .filter(row => row.ppid === appPid && /\/opencode serve --stdio\b/.test(row.command));
  const out = [];
  for (const row of rows) {
    const cwd = (spawnSync('lsof', ['-a', '-p', String(row.pid), '-d', 'cwd', '-Fn'], {encoding: 'utf8'}).stdout.match(/^n(.*)$/m) ?? [])[1];
    if (cwd && fs.existsSync(cwd) && fs.realpathSync(cwd) === want) out.push({...row, cwd});
  }
  return out;
}

function signalServer(s, server, signal) {
  const current = spawnSync('ps', ['-p', String(server.pid), '-o', 'ppid=,command='], {encoding: 'utf8'}).stdout.trim();
  assert.ok(current.startsWith(`${s.d.identity.pid} `) && /\/opencode serve --stdio\b/.test(current), `server ${server.pid} is no longer the QA app's opencode child: ${current}`);
  s.evidence.command({method: 'operator-signal', params: {pid: server.pid, signal, cwd: server.cwd}, phase: s.evidence.phase, at: now()});
  process.kill(server.pid, signal);
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

// What the user sees for `role` right now: session status, last assistant
// entry status and its part kinds, open cards, provider notice.
function userView(s, role) {
  const chat = s.d.chatByRole(role);
  const last = chat.mirror.entries.filter(e => e.role === 'assistant').at(-1);
  return {at: now(), status: s.d.sessions.status(chat.id), lastStatus: last?.status ?? null,
    lastParts: (last?.parts ?? []).map(p => p.kind === 'tool' ? `tool:${p.call?.kind}:${p.resolved ? 'resolved' : 'open'}${p.isError ? ':error' : ''}` : p.kind === 'input' ? `input:${p.resolved ? 'resolved' : 'open'}` : p.kind),
    lastText: textOf(last ?? {}).slice(-240), openInputs: chat.mirror.pendingInputs().map(p => p.requestId),
    providerNotice: chat.mirror.latest.providerNotice ?? null};
}

// Samples userView every `everyMs` until `untilFn()` or `ms` elapse.
async function timeline(s, role, ms, everyMs = 5000, untilFn = () => false) {
  const rows = [];
  const end = Date.now() + ms;
  while (Date.now() < end) {
    rows.push(userView(s, role));
    if (untilFn()) break;
    await sleep(Math.min(everyMs, Math.max(1, end - Date.now())));
  }
  rows.push(userView(s, role));
  return rows;
}
const SUBAGENT_TERMINAL = new Set(['done', 'failed', 'interrupted', 'unobserved']);
const subagentParts = entries => entries.flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool' && p.call?.kind === 'unknown' && /^Agent(:|$)/.test(p.call.name ?? '')));
const lunaParts = entries => entries.flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool' && p.call?.kind === 'mcp' && p.call.server === 'luna'));
const allowAll = pending => (pending.questions.some(q => q.approval) ? {approval: 'allow-once'} : null);

// Subscribes to the nested transcript of every spawn chip (after
// `sinceMessageId` when given); an already attached child keeps its role.
async function attachChildren(s, parentRole, prefix, sinceMessageId = null) {
  const scope = sinceMessageId ? s.after(parentRole, sinceMessageId) : s.entries(parentRole);
  const roles = [];
  for (const part of subagentParts(scope)) {
    if (!part.subagentRef) continue;
    const existing = s.d.chats.get(part.subagentRef);
    if (existing) {
      roles.push(existing.role);
      continue;
    }
    const role = `${prefix}-${[...s.d.chats.values()].filter(c => c.role.startsWith(`${prefix}-`)).length + 1}`;
    const parent = s.d.chatByRole(parentRole);
    s.d.attach(part.subagentRef, {role, config: parent.config, cwd: parent.cwd, spaceId: parent.spaceId});
    await Promise.race([s.d.chatByRole(role).mirror.ready, new Promise(resolve => setTimeout(resolve, 10000))]);
    roles.push(role);
  }
  return roles;
}

// ------------------------------------------------------------------ scenarios
const SCENARIOS = {
  S1: {
    title: 'Approvals and questions', validation: ['OCE-16', 'OCE-05'], parity: true, permissionMode: 'ask', phases: ['run'],
    steps: ['allow-once', 'deny-with-feedback', 'cancel-turn', 'question-single', 'question-multi', 'question-free-text', 'stale-answer-after-stop'],
    // The commands run an interpreter so no provider auto-allows them: Claude
    // Code runs `echo` and in-workspace file writes without asking in
    // acceptEdits, which LUNA's ask mode maps to. Both providers must ask.
    prompts: n => ({
      allow: `Use your shell tool to run exactly this command: python3 -c "print('S1-ALLOW-${n}')"\nThen reply with only the command's output. Do not run any other command.`,
      deny: `Use your shell tool to run exactly this command: python3 -c "print('S1-DENY-${n}')"\nIf you are not allowed to run it, do not retry; reply with exactly S1-DENIED-${n}.`,
      cancel: `Use your shell tool to run exactly this command: python3 -c "print('S1-CANCEL-${n}')"\nThen reply with only the command's output.`,
      single: `${BLOCKING_QUESTION} Use your tool for asking the user a multiple-choice question (do not ask in plain text). Ask me to choose exactly one color from these options: red, green, blue. After I answer, reply with exactly COLOR-${n}-<the color I chose>.`,
      multi: `${BLOCKING_QUESTION} Use your tool for asking the user a multiple-choice question that allows multiple selections (do not ask in plain text). Ask me to choose one or more of these options: alpha, beta, gamma. After I answer, reply with exactly PICKED-${n}- followed by my choices joined with "+".`,
      stale: `Use your shell tool to run exactly this command: python3 -c "print('S1-STALE-${n}')"\nThen reply with only the command's output.`,
      free: `${BLOCKING_QUESTION} Use your tool for asking the user a question (do not ask in plain text). Ask me for my favourite colour, offering red, green and blue as options and letting me type my own answer instead. After I answer, reply with exactly FREE-${n}- followed by my answer in lowercase.`,
    }),
    unverified: ['MCP elicitation forms need a mock MCP server fixture (not part of this runner); the answerer supports them (buildAnswers mcp policy).',
      'Question steps at T2 need a scripted mock entry; run them at T3.'],
    async run(s, P) {
      const root = 'root';
      await s.step('allow-once', async () => {
        let card = null;
        const t = await s.turn(root, P.allow, {mock: `tool-shell:echo ${s.m('S1-ALLOW')}`, answer: async pending => {
          card ??= pending;
          await s.d.holdForCapture('s1-approval-card');
          return {approval: 'allow-once'};
        }});
        s.check('approval-shown', 'an approval card reached the shared panel', card ? card.questions.map(q => q.approval?.actionKind ?? 'question') : null, Boolean(card?.questions.some(q => q.approval)));
        s.check('allow-once-sent', 'Allow once answered through respondApproval', t.wait.handled, t.wait.handled.some(h => h.status === 'applied'));
        s.settled(t.wait);
        const tools = s.toolsAfter(root, t.messageId);
        s.check('tool-ran', 'a resolved, non-error tool part follows the approval', tools.map(x => `${x.call?.kind}:${x.resolved}:${x.isError}`), tools.some(x => x.resolved && !x.isError));
        s.modelCheck('allow-output', `reply carries S1-ALLOW-${s.nonce}`, t.reply.slice(0, 200), t.reply.includes(s.m('S1-ALLOW')) || markerInTools(tools, s.m('S1-ALLOW')));
      });
      await s.step('deny-with-feedback', async () => {
        const t = await s.turn(root, P.deny, {mock: `tool-shell:echo ${s.m('S1-DENY')}`, answer: () => ({approval: 'deny'})});
        s.check('deny-sent', 'Deny answered through respondApproval', t.wait.handled, t.wait.handled.some(h => h.status === 'applied'));
        s.settled(t.wait, 'deny-continues', ['idle']);
        s.check('deny-blocked-tool', 'no successful tool output with the denied marker', null, !markerInTools(s.toolsAfter(root, t.messageId), s.m('S1-DENY')));
        s.modelCheck('deny-feedback-reply', `the model continued and replied S1-DENIED-${s.nonce}`, t.reply.slice(0, 200), t.reply.includes(s.m('S1-DENIED')));
      });
      await s.step('cancel-turn', async () => {
        const t = await s.turn(root, P.cancel, {mock: `tool-shell:echo ${s.m('S1-CANCEL')}`, answer: () => ({approval: 'cancel'})});
        s.check('cancel-sent', 'Cancel answered through respondApproval', t.wait.handled, t.wait.handled.some(h => h.status === 'applied'));
        // Unified cancel outcome: every provider's cancelled turn ends Idle
        // (never Errored) with its entry stamped aborted.
        s.settled(t.wait, 'cancel-stops-turn', ['idle']);
        s.check('cancel-blocked-tool', 'no successful tool output with the cancelled marker', null, !markerInTools(s.toolsAfter(root, t.messageId), s.m('S1-CANCEL')));
        const last = s.after(root, t.messageId).filter(e => e.role === 'assistant').at(-1);
        s.check('cancel-entry-aborted', 'the cancelled turn is stamped aborted, the same for every provider', last?.status ?? null, last?.status === 'aborted');
        s.check('cancel-not-errored', 'no errored status was published for the cancelled turn', t.wait.statuses, !(t.wait.statuses ?? []).includes('errored'));
      });
      await s.step('question-single', async () => {
        let shown = null;
        const t = await s.turn(root, P.single, {answer: async pending => {
          if (pending.questions.some(q => q.approval)) return {approval: 'allow-once'};
          shown ??= pending;
          await s.d.holdForCapture('s1-question-panel');
          return {pick: 'green'};
        }});
        s.modelCheck('question-shown', 'the question reached the shared question panel (no approval)', shown?.questions.map(q => ({options: q.options, multi: q.multiSelect})) ?? null, Boolean(shown));
        s.settled(t.wait);
        s.modelCheck('question-answer-consumed', `reply is COLOR-${s.nonce}-green`, t.reply.slice(0, 200), t.reply.toLowerCase().includes(`color-${s.nonce}-green`.toLowerCase()));
      });
      await s.step('question-multi', async () => {
        let shown = null;
        const t = await s.turn(root, P.multi, {answer: pending => {
          if (pending.questions.some(q => q.approval)) return {approval: 'allow-once'};
          shown ??= pending;
          return {pick: ['alpha', 'gamma']};
        }});
        s.modelCheck('multi-question-shown', 'the question reached the shared panel', shown?.questions.map(q => ({options: q.options, multi: q.multiSelect})) ?? null, Boolean(shown));
        s.info('multi-select-flag', 'the provider marked the question multi-select', shown?.questions.map(q => q.multiSelect) ?? null, Boolean(shown?.questions.some(q => q.multiSelect)));
        s.settled(t.wait);
        s.modelCheck('multi-answer-consumed', `reply names alpha and gamma (PICKED-${s.nonce}-...)`, t.reply.slice(0, 200), /alpha/i.test(t.reply) && /gamma/i.test(t.reply));
      });
      await s.step('question-free-text', async () => {
        let shown = null;
        const t = await s.turn(root, P.free, {answer: pending => {
          if (pending.questions.some(q => q.approval)) return {approval: 'allow-once'};
          shown ??= pending;
          return {text: 'teal'};
        }});
        s.modelCheck('free-question-shown', 'the question reached the shared panel', shown?.questions.map(q => ({options: q.options, multi: q.multiSelect, input: q.input ?? null})) ?? null, Boolean(shown));
        if (shown) s.check('free-text-offered', 'the question panel offers a typed answer (accepts_typed_answer: no options, no input policy, or allowOther)', shown?.questions.map(q => acceptsTypedAnswer(q)) ?? null, Boolean(shown?.questions.some(acceptsTypedAnswer)));
        s.settled(t.wait);
        s.modelCheck('free-text-consumed', `reply is FREE-${s.nonce}-teal (the typed answer reached the model)`, t.reply.slice(0, 200), t.reply.toLowerCase().includes(`free-${s.nonce}-teal`.toLowerCase()));
      });
      await s.step('stale-answer-after-stop', async () => {
        const chatId = s.chatId(root);
        const sent = await s.d.send(chatId, s.p(P.stale, `tool-shell:echo ${s.m('S1-STALE')}`), {label: 'message'});
        const pending = await s.d.waitPending(chatId);
        s.check('stale-approval-shown', 'an approval is pending before Stop', pending.requestId, Boolean(pending.questions.some(q => q.approval)));
        const stopped = await s.d.stop(chatId);
        s.check('stop-applied', 'Stop receipt applied', stopped.receipt?.status, stopped.receipt?.status === 'applied');
        const mirror = s.d.chatByRole(root).mirror;
        await mirror.waitFor(() => mirror.inputResolved(pending.requestId) || !mirror.pendingInputs().some(p => p.requestId === pending.requestId),
          {timeoutMs: 60000, label: 'approval closed by Stop'}).catch(() => null);
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, answer: null, ignore: [pending.requestId], timeoutMs: 60000});
        s.settled(wait, 'stopped-turn-settles', ['idle', 'errored']);
        s.check('stale-card-closed', 'Stop closes the pending approval (resolved or gone)', {resolved: mirror.inputResolved(pending.requestId), stillPending: mirror.pendingInputs().some(p => p.requestId === pending.requestId), wait: wait.result},
          mirror.inputResolved(pending.requestId) || !mirror.pendingInputs().some(p => p.requestId === pending.requestId));
        let probe;
        try {
          probe = (await s.d.answer(chatId, pending, {approval: 'allow-once'}, {probe: true})).receipt;
        } catch (error) {
          probe = {status: error instanceof NotAnswerable ? 'not-answerable' : 'rpc-error', reason: {message: String(error.message ?? error)}};
        }
        s.info('stale-probe-receipt', 'the late answer is refused or has no effect (receipt recorded)', {status: probe?.status, reason: probe?.reason?.code ?? probe?.reason?.message ?? null}, probe?.status !== 'applied');
        const quiet = await s.stays(root, entries => markerInTools(entries.flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool')), s.m('S1-STALE')), 15000);
        s.check('stale-answer-no-effect', 'no tool output with the stale marker appears in 15 s after the late answer (OCF-41/50)', quiet, quiet);
        s.check('stale-not-working', 'the chat is not left working', s.d.sessions.status(chatId), !s.d.busy(chatId));
      });
    },
  },

  S2: {
    title: 'Native subagent attribution and background work', validation: ['OCE-01', 'OCE-21'], parity: true, permissionMode: 'full', phases: ['run'],
    steps: ['subagents', 'background-shell', 'stop-with-running-child'],
    prompts: n => ({
      subagents: `Use your subagent tool (the tool that starts a separate agent) to start two subagents. ${SHELL_AGENT} Subagent A runs in the foreground: tell it to run the shell command "echo CHILD-A-${n}" and report the output. Subagent B: start it in the background if your subagent tool supports background agents, otherwise in the foreground; tell it to run the shell command "echo CHILD-B-${n}" and report the output. Do not run either command yourself. When both have been started, reply with exactly ROOT-DONE-${n}.`,
      background: `Use your shell tool's background option to run this command in the background: sleep 8; echo BG-SHELL-${n}\nDo not wait for it to finish. Immediately reply with exactly BG-STARTED-${n}.`,
      slow: `Use your subagent tool to start one subagent (${SHELL_AGENT}) and tell it to run the shell command "sleep 45; echo CHILD-SLOW-${n}" and report the output. Wait for it, then reply with its output.`,
    }),
    unverified: ['Grandchild visibility (experimental.subagent_depth 2) needs that profile setting; not set by this runner.'],
    async run(s, P) {
      await s.step('subagents', async () => {
        const t = await s.turn('root', P.subagents, {mock: 'subagent', answer: allowAll});
        s.settled(t.wait);
        const rootChat = s.d.chatByRole('root');
        // Background children may finish after the root turn: wait on frames.
        await rootChat.mirror.waitFor(entries => subagentParts(entries).length && subagentParts(entries).every(p => SUBAGENT_TERMINAL.has(p.subagentStatus)),
          {timeoutMs: 180000, label: 'subagents settled'}).catch(() => null);
        const spawns = subagentParts(s.entries('root'));
        s.check('spawns-in-root', 'root shows native subagent spawn chips (Agent: ...)', spawns.map(p => ({status: p.subagentStatus ?? null, ref: Boolean(p.subagentRef)})), spawns.length >= 1);
        const children = await attachChildren(s, 'root', 'child');
        s.check('child-docs', 'each spawn links a nested subagent transcript', children, children.length === spawns.length && children.length >= 1);
        const childTools = children.flatMap(role => s.entries(role).flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool')));
        const rootTools = s.entries('root').flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool'));
        for (const marker of ['CHILD-A', 'CHILD-B']) {
          s.modelCheck(`${marker.toLowerCase()}-in-child`, `${marker}-${s.nonce} output appears under a child transcript`, markerInTools(childTools, s.m(marker)), markerInTools(childTools, s.m(marker)));
          s.check(`${marker.toLowerCase()}-not-in-root-tools`, 'no root tool output carries the child marker (OCF-21)', markerInTools(rootTools, s.m(marker)), !markerInTools(rootTools, s.m(marker)));
        }
        const ids = [...s.d.chats.values()].flatMap(c => c.mirror.entries.map(e => e.id));
        s.check('entry-ids-unique', 'entry ids are unique across root and child documents (OCF-22)', `${ids.length} ids`, new Set(ids).size === ids.length);
        s.modelCheck('root-done', `root replied ROOT-DONE-${s.nonce}`, t.reply.slice(0, 200), t.reply.includes(s.m('ROOT-DONE')));
        const extraTurns = s.after('root', t.messageId).filter(e => e.role === 'assistant').length;
        s.info('wake-turn', 'a background completion appears as a provider-owned turn (assistant entry without a user prompt)', extraTurns, extraTurns >= 1);
        s.d.capture('s2-subagents');
      });
      await s.step('background-shell', async () => {
        const t = await s.turn('root', P.background, {mock: 'tool-shell-background', answer: allowAll});
        s.settled(t.wait);
        const chat = s.d.chatByRole('root');
        const live = await waitOn([chat.nativeTasks], () => (chat.nativeTasks.latest?.tasks ?? []).find(task => task.kind === 'shell'), 20000, 'background shell row').catch(() => null);
        // WatchNativeTasks stays empty for both providers (seen live); the
        // shared surface for the row is the task work panel, read once here.
        const work = await s.d.rpc.call('GetTaskWork', {taskId: s.chatId('root')}).catch(error => ({error: String(error.message ?? error)}));
        s.evidence.save('task-work-background-shell.json', work);
        const row = (work.items ?? []).find(item => item.kind === 'shell' && /background/i.test(item.title ?? ''));
        s.info('background-shell-native-task', 'WatchNativeTasks reports the shell (NativeTaskChanged{Shell})', live ? {kind: live.kind, status: live.status ?? null} : null, Boolean(live));
        s.check('background-shell-row', 'the background shell shows as the shared task-work row (kind shell)', row ? {kind: row.kind, state: row.state, title: row.title} : (work.items ?? []).map(i => ({kind: i.kind, state: i.state})), Boolean(row || live));
        s.modelCheck('background-started-reply', `reply BG-STARTED-${s.nonce}`, t.reply.slice(0, 200), t.reply.includes(s.m('BG-STARTED')));
      });
      await s.step('stop-with-running-child', async () => {
        const chatId = s.chatId('root');
        const sent = await s.d.send(chatId, s.p(P.slow, 'subagent'));
        const mirror = s.d.chatByRole('root').mirror;
        const spawned = await mirror.waitFor(entries => {
          const at = entries.findIndex(e => e.id === sent.messageId);
          return at >= 0 && subagentParts(entries.slice(at + 1)).find(p => p.subagentStatus === 'running');
        }, {timeoutMs: 120000, label: 'running subagent'}).catch(() => null);
        s.check('child-running', 'a running subagent chip appears before Stop', spawned?.subagentStatus ?? null, Boolean(spawned));
        const stopped = await s.d.stop(chatId);
        s.check('stop-applied', 'Stop receipt applied', stopped.receipt?.status, stopped.receipt?.status === 'applied');
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, timeoutMs: 90000});
        s.settled(wait, 'stopped-settles', ['idle', 'errored']);
        const childRoles = await attachChildren(s, 'root', 'slow-child', sent.messageId);
        const leaked = () => s.after('root', sent.messageId).some(e => e.role === 'assistant' && textOf(e).includes(s.m('CHILD-SLOW')))
          || childRoles.some(role => markerInTools(s.entries(role).flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool')), s.m('CHILD-SLOW')));
        const quiet = (await s.stays('root', leaked, 20000)) && !leaked();
        s.check('child-stopped', 'the slow child marker never arrives after Stop (OCF-40)', quiet, quiet);
        const chip = subagentParts(s.after('root', sent.messageId))[0];
        s.info('child-chip-final', 'the child chip ends interrupted or terminal, not running', chip?.subagentStatus ?? null, SUBAGENT_TERMINAL.has(chip?.subagentStatus));
      });
    },
  },

  S3: {
    title: 'LUNA tool delivery', validation: ['OCE-18'], parity: true, permissionMode: 'full', phases: ['run'],
    steps: ['fresh-call', 'resumed-call', 'subagent-refused', 'forked-call', 'post-compaction-call', 'cross-project'],
    prompts: n => ({
      luna: `Call the get_workspace tool from the luna MCP server (the LUNA workspace tool). Then reply with exactly WS-${n}- followed by the project name it reports. Do not use any other tool.`,
      child: 'Use your subagent tool to start one subagent. Tell it to call the get_workspace tool from the luna MCP server and to report whether the call succeeded or was refused. Do not call any luna tool yourself. Reply with the subagent\'s report.',
      cross: `Using the luna MCP tools, try to read the workspace of a different LUNA project named does-not-exist-${n}. Report exactly whether the call was allowed or refused. Do not change anything.`,
    }),
    unverified: ['Instruction-entry readback (bootstrap size, instructions channel) needs native server readback; LUNA keeps the server password private.',
      'Re-attachment after an OpenCode server restart is covered by S4 (app restart), not here.'],
    async run(s, P) {
      const lunaOk = (role, messageId) => lunaParts(s.after(role, messageId)).filter(p => p.call.tool === 'get_workspace');
      await s.step('fresh-call', async () => {
        const t = await s.turn('root', P.luna, {mock: 'text', answer: allowAll});
        s.settled(t.wait);
        const calls = lunaOk('root', t.messageId);
        s.modelCheck('luna-tool-fresh', 'mcp:luna/get_workspace resolved without error on a fresh session', calls.map(p => `${p.resolved}:${p.isError}`), calls.some(p => p.resolved && !p.isError));
      });
      await s.step('resumed-call', async () => {
        const t = await s.turn('root', P.luna, {mock: 'text', answer: allowAll});
        s.settled(t.wait);
        const calls = lunaOk('root', t.messageId);
        s.modelCheck('luna-tool-resumed', 'mcp:luna/get_workspace resolved on the resumed session', calls.map(p => `${p.resolved}:${p.isError}`), calls.some(p => p.resolved && !p.isError));
      });
      await s.step('subagent-refused', async () => {
        const t = await s.turn('root', P.child, {mock: 'subagent', answer: allowAll});
        s.settled(t.wait);
        await s.d.chatByRole('root').mirror.waitFor(entries => subagentParts(entries).every(p => SUBAGENT_TERMINAL.has(p.subagentStatus)), {timeoutMs: 120000, label: 'child settled'}).catch(() => null);
        const children = await attachChildren(s, 'root', 'luna-child', t.messageId);
        const childLuna = children.flatMap(role => lunaParts(s.entries(role)));
        s.check('subagent-luna-refused', 'no successful luna tool call from a native subagent (root-only, OCF-121)', childLuna.map(p => `${p.call.tool}:${p.resolved}:${p.isError}`), !childLuna.some(p => p.resolved && !p.isError));
        s.check('root-did-not-call', 'the root made no luna call in this step', lunaParts(s.after('root', t.messageId)).length, lunaParts(s.after('root', t.messageId)).length === 0);
      });
      await s.step('forked-call', async () => {
        const fork = await s.d.fork(s.chatId('root'), {boundary: 'tip', role: 'fork-tip'});
        s.check('fork-applied', 'tip fork applied', fork.receipt?.status, fork.receipt?.status === 'applied');
        if (fork.receipt?.status !== 'applied') return;
        const t = await s.turn('fork-tip', P.luna, {mock: 'text', answer: allowAll});
        s.settled(t.wait);
        const calls = lunaOk('fork-tip', t.messageId);
        s.modelCheck('luna-tool-forked', 'mcp:luna/get_workspace resolved in the forked session', calls.map(p => `${p.resolved}:${p.isError}`), calls.some(p => p.resolved && !p.isError));
      });
      await s.step('post-compaction-call', async () => {
        const before = JSON.stringify(s.d.chatByRole('root').mirror.latest.contextControl ?? null);
        const compact = await s.d.compact(s.chatId('root'));
        s.check('compact-receipt', 'compactContext receipt applied (or a truthful refusal)', {status: compact.receipt?.status, reason: compact.receipt?.reason?.code ?? null}, ['applied', 'rejected'].includes(compact.receipt?.status));
        const mirror = s.d.chatByRole('root').mirror;
        await mirror.waitFor(() => JSON.stringify(mirror.latest.contextControl ?? null) !== before, {timeoutMs: 120000, label: 'context control change'}).catch(() => null);
        s.info('compaction-lifecycle', 'the document context control changed after compaction', mirror.latest.contextControl, JSON.stringify(mirror.latest.contextControl ?? null) !== before);
        const t = await s.turn('root', P.luna, {mock: 'text', answer: allowAll});
        s.settled(t.wait);
        const calls = lunaOk('root', t.messageId);
        s.modelCheck('luna-tool-post-compaction', 'LUNA tools survive compaction (OCF-111)', calls.map(p => `${p.resolved}:${p.isError}`), calls.some(p => p.resolved && !p.isError));
      });
      await s.step('cross-project', async () => {
        const t = await s.turn('root', P.cross, {mock: 'text', answer: allowAll});
        s.settled(t.wait);
        const calls = lunaParts(s.after('root', t.messageId));
        s.info('cross-project-calls', 'luna calls in a cross-project attempt (refusals are errors)', calls.map(p => `${p.call.tool}:${p.isError}`), true);
        s.modelCheck('cross-project-reported', 'the agent reports the cross-project request as refused', t.reply.slice(0, 200), /refus|denied|not allowed|unavailable|cannot|can't/i.test(t.reply));
      });
    },
  },

  S4: {
    title: 'Restart with a pending approval and a queued row', validation: ['OCE-04', 'OCE-05', 'OCE-09'], parity: false, permissionMode: 'ask', phases: ['pre', 'post'],
    steps: ['completed-turn', 'pending-approval', 'queued-row', '(operator restarts the QA app)', 'after-restart-readback', 'stale-or-live-answer', 'continue-after-restart'],
    prompts: n => ({
      before: `Reply with exactly S4-BEFORE-${n}. Do not use tools.`,
      pending: `Use your shell tool to run exactly this command: python3 -c "print('S4-PENDING-${n}')"\nThen reply with only the command's output.`,
      queued: `Reply with exactly S4-QUEUED-${n}. Do not use tools.`,
      after: `Reply with exactly S4-AFTER-${n}. Do not use tools.`,
    }),
    unverified: ['SSE break and server restart mid-turn without an app restart (OCE-04 a-f) need a fault-injecting proxy; this scenario restarts the whole app.',
      'Managed OpenCode service resume (OCE-09) needs a second OpenCode process on the isolated storage.'],
    async pre(s, P) {
      await s.step('completed-turn', async () => {
        const t = await s.turn('root', P.before, {mock: `text:${s.m('S4-BEFORE')}`});
        s.settled(t.wait);
        s.modelCheck('before-reply', `reply S4-BEFORE-${s.nonce}`, t.reply.slice(0, 120), t.reply.includes(s.m('S4-BEFORE')));
      });
      await s.step('pending-approval', async () => {
        const chatId = s.chatId('root');
        const sent = await s.d.send(chatId, s.p(P.pending, `tool-shell:echo ${s.m('S4-PENDING')}`));
        const pending = await s.d.waitPending(chatId);
        s.ids.pending = {requestId: pending.requestId, messageId: sent.messageId, questions: pending.questions};
        s.check('pending-approval', 'an approval is pending before the restart', pending.requestId, Boolean(pending.questions.some(q => q.approval)));
        s.d.capture('s4-pending-before-restart');
      });
      await s.step('queued-row', async () => {
        const chatId = s.chatId('root');
        const sent = await s.d.send(chatId, s.p(P.queued, `text:${s.m('S4-QUEUED')}`), {queue: true});
        s.ids.queued = {messageId: sent.messageId};
        const queue = s.d.chatByRole('root').queue;
        const row = await waitOn([queue], () => (queue.latest?.items ?? []).find(i => i.id === sent.messageId), 30000, 'queued row').catch(() => null);
        s.check('queued-row', 'the queued row is visible in WatchQueue', row ? {id: row.id} : null, Boolean(row));
      });
      s.evidence.manifest.restart = {requiredAt: now(), previousPid: s.d.identity.pid, pending: s.ids.pending, queued: s.ids.queued};
      s.evidence.save('restart-required.json', {
        instructions: 'QA operator: let the QA app save and quit gracefully, relaunch the same QA bundle (never force-quit; never the primary LUNA Dev), then run this scenario with --phase post --run <this dir>.',
        previousPid: s.d.identity.pid, pending: s.ids.pending, queued: s.ids.queued, at: now(),
      });
    },
    async post(s, P) {
      const previous = s.evidence.manifest.restart;
      await s.step('after-restart-readback', async () => {
        s.check('app-restarted', 'a different QA app pid than the pre phase (QF-24)', `${previous?.previousPid} -> ${s.d.identity.pid}`, previous?.previousPid && previous.previousPid !== s.d.identity.pid);
        const entries = s.entries('root');
        for (const send of s.evidence.manifest.sends.filter(x => x.kind === 'message' && x.messageId && x.phase === 'pre')) {
          const count = entries.filter(e => e.id === send.messageId).length;
          const queued = send.messageId === previous?.queued?.messageId;
          s.check(`send-${send.index}-once`, queued ? 'the queued row is delivered at most once' : 'the user message survives restart exactly once', count, queued ? count <= 1 : count === 1);
        }
        const cards = entries.flatMap(e => (e.parts ?? []).filter(p => p.kind === 'input' && p.requestId === previous?.pending?.requestId));
        s.check('no-duplicate-approval', 'the pre-restart approval appears at most once (OCF-58)', cards.length, cards.length <= 1);
        s.info('approval-after-restart', 'pre-restart approval state after restart (provider truth: re-shown once or closed)', cards.map(c => ({resolved: c.resolved})), true);
        const queue = s.d.chatByRole('root').queue;
        s.info('queue-after-restart', 'queued row state after restart (expected: paused, not auto-replayed)', (queue.latest?.items ?? []).map(i => i.id === previous?.queued?.messageId ? 'pre-restart-row' : 'other'), true);
        s.d.capture('s4-after-restart');
      });
      await s.step('stale-or-live-answer', async () => {
        const chatId = s.chatId('root');
        const mirror = s.d.chatByRole('root').mirror;
        const live = mirror.pendingInputs().find(p => p.requestId === previous?.pending?.requestId);
        const pending = live ?? {requestId: previous?.pending?.requestId, questions: previous?.pending?.questions ?? []};
        let receipt;
        try {
          receipt = (await s.d.answer(chatId, pending, {approval: 'allow-once'}, {probe: !live})).receipt;
        } catch (error) {
          receipt = {status: 'rpc-error', reason: {message: String(error.message ?? error)}};
        }
        s.info('answer-after-restart', live ? 'the re-shown approval is answerable once' : 'the stale approval answer is refused or has no effect', {live: Boolean(live), status: receipt?.status, reason: receipt?.reason?.code ?? receipt?.reason?.message ?? null}, live ? receipt?.status === 'applied' : receipt?.status !== 'applied');
        if (!live) {
          const quiet = await s.stays('root', entries => markerInTools(entries.flatMap(e => (e.parts ?? []).filter(p => p.kind === 'tool')), s.m('S4-PENDING')), 15000);
          s.check('stale-no-effect', 'no tool output with the pre-restart marker after the stale answer', quiet, quiet);
        }
      });
      await s.step('continue-after-restart', async () => {
        // turn() waits for any recovered or queued turn to finish first.
        const t = await s.turn('root', P.after, {mock: `text:${s.m('S4-AFTER')}`, answer: allowAll});
        s.settled(t.wait);
        s.modelCheck('after-reply', `reply S4-AFTER-${s.nonce}`, t.reply.slice(0, 120), t.reply.includes(s.m('S4-AFTER')));
        const queuedCount = s.entries('root').filter(e => e.id === previous?.queued?.messageId).length;
        s.check('queued-once-overall', 'the pre-restart queued message is never duplicated', queuedCount, queuedCount <= 1);
      });
    },
  },

  S5: {
    title: 'History: fork, edit, rewind and side chats', validation: ['OCE-10', 'OCE-24'], parity: true, permissionMode: 'full', phases: ['run'],
    steps: ['three-marker-turns', 'fork-user-row', 'fork-assistant-row', 'fork-continuation', 'side-chat-native', 'side-chat-snapshot', 'edit-earlier-message', 'rewind', 'rewind-continuation', 'reopen-readback'],
    prompts: n => ({
      marker: i => `Remember this marker: M${i}-${n}. Reply with exactly M${i}-${n}. Do not use tools.`,
      list: 'List every marker I asked you to remember in this conversation, separated by single spaces. Reply only with the markers. Do not use tools.',
      sideRecall: 'Which marker did I ask you to remember in my second message? Reply with only that marker. Do not use tools.',
      sideNative: `Reply with exactly SIDE-NATIVE-${n}. Do not use tools.`,
      sideSnap: `Reply with exactly SIDE-SNAP-${n}. Do not use tools.`,
      sideSeed: 'What text did I select when I opened this side chat? Reply with only that text. Do not use tools.',
      edit: `Remember this marker: E2-${n}. Reply with exactly E2-${n}. Do not use tools.`,
    }),
    unverified: ['Native message-list readback for source and destination (LUNA keeps the server private); the wire log slice holds the fork/revert requests.',
      'Import of a TUI-created session with the plan agent (OCE-10) needs an external OpenCode client on the isolated profile.'],
    async run(s, P) {
      const ids = {users: [], replies: []};
      await s.step('three-marker-turns', async () => {
        for (const i of [1, 2, 3]) {
          const t = await s.turn('root', P.marker(i), {mock: `text:${s.m(`M${i}`)}`});
          s.settled(t.wait, `marker-${i}-settled`);
          s.modelCheck(`marker-${i}-reply`, `reply M${i}-${s.nonce}`, t.reply.slice(0, 120), t.reply.includes(s.m(`M${i}`)));
          ids.users.push(t.messageId);
          ids.replies.push(s.after('root', t.messageId).filter(e => e.role === 'assistant').at(-1)?.id ?? null);
        }
        s.check('three-turns', 'three user and three assistant rows', {users: s.users('root').length}, s.users('root').length === 3 && ids.replies.every(Boolean));
      });
      const sourceIds = () => s.entries('root').map(e => e.id).join(',');
      await s.step('fork-user-row', async () => {
        const before = sourceIds();
        const fork = await s.d.fork(s.chatId('root'), {boundary: 'beforeUserMessage', messageId: ids.users[1], role: 'fork-user'});
        s.check('fork-user-applied', 'fork before user row 2 applied', {status: fork.receipt?.status, reason: fork.receipt?.reason?.message ?? null}, fork.receipt?.status === 'applied');
        if (fork.receipt?.status === 'applied') {
          const users = s.users('fork-user');
          s.check('fork-user-prefix', 'the fork holds only turn 1', users.map(textOf).map(t => maskNonce(t, s.nonce)), users.length === 1 && textOf(users[0]).includes(s.m('M1')));
        }
        s.check('source-unchanged', 'the source transcript is unchanged by the fork (OCF-92)', null, sourceIds() === before);
      });
      await s.step('fork-assistant-row', async () => {
        const before = sourceIds();
        const fork = await s.d.fork(s.chatId('root'), {boundary: 'afterCompletedTurn', messageId: ids.replies[1], role: 'fork-assistant'});
        s.check('fork-assistant-applied', 'fork after assistant row 2 applied', {status: fork.receipt?.status, reason: fork.receipt?.reason?.message ?? null}, fork.receipt?.status === 'applied');
        if (fork.receipt?.status === 'applied') {
          const users = s.users('fork-assistant');
          s.check('fork-assistant-prefix', 'the fork holds turns 1 and 2', users.length, users.length === 2);
        }
        s.check('source-unchanged', 'the source transcript is unchanged by the fork', null, sourceIds() === before);
      });
      await s.step('fork-continuation', async () => {
        if (!s.d.chatByRole('fork-user')) return s.check('fork-continuation', 'needs the user-row fork', 'no fork', false);
        const t = await s.turn('fork-user', P.list, {mock: `text:${s.m('M1')}`});
        s.settled(t.wait);
        s.modelCheck('fork-recall', 'the fork recalls M1 but not M2/M3 (real msg_ boundary, OCF-155)', t.reply.slice(0, 200),
          t.reply.includes(s.m('M1')) && !t.reply.includes(s.m('M2')) && !t.reply.includes(s.m('M3')));
      });
      await s.step('side-chat-native', async () => {
        const before = sourceIds();
        const side = await s.d.createSideChat(s.chatId('root'), {role: 'side-native', snapshot: false, originMessageId: ids.replies[1]});
        s.check('side-native-created', 'native-fork side chat created', side.error ?? side.created?.meta?.contextMode ?? null, !side.error);
        if (side.error) return;
        s.info('side-native-mode', 'the side chat context mode is nativeFork', side.created?.meta?.contextMode ?? null, side.created?.meta?.contextMode === 'nativeFork');
        const first = await s.waitSideTurn('side-native', await s.d.sendSideChat(side.id, s.p(P.sideRecall, `text:${s.m('M2')}`)));
        s.settled(first.wait, 'side-native-turn-1');
        s.modelCheck('side-native-recall', 'the native fork knows the parent context (M2)', first.reply.slice(0, 120), first.reply.includes(s.m('M2')));
        const second = await s.waitSideTurn('side-native', await s.d.sendSideChat(side.id, s.p(P.sideNative, `text:${s.m('SIDE-NATIVE')}`)));
        s.settled(second.wait, 'side-native-turn-2');
        s.check('parent-unchanged', 'the parent transcript is unchanged by the side chat', null, sourceIds() === before);
        s.evidence.save('side-native-readback.json', await s.d.rpc.call('ReadSideChat', {id: side.id}).catch(error => ({error: String(error.message ?? error)})));
        s.d.capture('s5-side-native');
      });
      await s.step('side-chat-snapshot', async () => {
        const before = sourceIds();
        const anchor = s.m('M2');
        const side = await s.d.createSideChat(s.chatId('root'), {role: 'side-snapshot', snapshot: true, originMessageId: ids.replies[1], anchorText: anchor});
        s.check('side-snapshot-created', 'snapshot side chat created', side.error ?? side.created?.meta?.contextMode ?? null, !side.error);
        if (side.error) return;
        const first = await s.waitSideTurn('side-snapshot', await s.d.sendSideChat(side.id, s.p(P.sideSnap, `text:${s.m('SIDE-SNAP')}`)));
        s.settled(first.wait, 'side-snapshot-turn-1');
        const firstUser = s.users('side-snapshot')[0];
        s.check('seed-not-in-user-text', 'the snapshot seed is not prepended to the first user message (instruction channel)', textOf(firstUser).slice(0, 200), firstUser && !textOf(firstUser).includes(anchor));
        const second = await s.waitSideTurn('side-snapshot', await s.d.sendSideChat(side.id, s.p(P.sideSeed, `text:${anchor}`)));
        s.settled(second.wait, 'side-snapshot-turn-2');
        s.modelCheck('side-snapshot-seed-known', 'the snapshot side chat knows the selected excerpt', second.reply.slice(0, 120), second.reply.includes(anchor));
        s.check('parent-unchanged', 'the parent transcript is unchanged by the snapshot side chat', null, sourceIds() === before);
      });
      await s.step('edit-earlier-message', async () => {
        const edit = await s.d.editEarlier(s.chatId('root'), ids.users[1], s.p(P.edit, `text:${s.m('E2')}`));
        s.check('edit-applied', 'edit earlier message applied', {status: edit.receipt?.status, reason: edit.receipt?.reason?.message ?? null}, edit.receipt?.status === 'applied');
        if (edit.receipt?.status !== 'applied') return;
        const wait = await s.d.waitTurn(s.chatId('root'), {mark: edit.mark, timeoutMs: s.turnTimeout});
        s.settled(wait, 'edit-turn-settled');
        const users = s.users('root').map(textOf);
        s.check('edit-history', 'history is turn 1 plus the edited turn 2; turn 3 is gone', users.map(t => maskNonce(t, s.nonce)), users.length === 2 && users[1].includes(s.m('E2')) && !users.some(t => t.includes(s.m('M3'))));
        const reply = s.entries('root').filter(e => e.role === 'assistant').map(textOf).join('\n');
        s.modelCheck('edit-reply', `reply E2-${s.nonce}`, reply.slice(-120), reply.includes(s.m('E2')));
      });
      await s.step('rewind', async () => {
        const edited = s.users('root').find(e => textOf(e).includes(s.m('E2')));
        if (!edited) return s.check('rewind-target', 'needs the edited user row', null, false);
        fs.appendFileSync(path.join(s.fixture.dir, 'README.md'), 'Unrelated modified tracked content.\n');
        fs.writeFileSync(path.join(s.fixture.dir, 'unrelated.txt'), 'Unrelated untracked content.\n');
        const before = fileHashes(s.fixture.dir, ['README.md', 'unrelated.txt']);
        const rewind = await s.d.rewind(s.chatId('root'), edited.id);
        s.check('rewind-applied', 'same-chat rewind applied', {status: rewind.receipt?.status, reason: rewind.receipt?.reason?.message ?? null}, rewind.receipt?.status === 'applied');
        const mirror = s.d.chatByRole('root').mirror;
        await mirror.waitFor(entries => !entries.some(e => e.id === edited.id), {timeoutMs: 30000, label: 'rewound row removed'}).catch(() => null);
        s.check('rewind-history', 'history keeps only turn 1', s.users('root').map(textOf).map(t => maskNonce(t, s.nonce)), s.users('root').length === 1);
        s.check('rewind-keeps-files', 'rewind keeps files (files:false, OCF-93)', fileHashes(s.fixture.dir, ['README.md', 'unrelated.txt']), JSON.stringify(fileHashes(s.fixture.dir, ['README.md', 'unrelated.txt'])) === JSON.stringify(before));
      });
      await s.step('rewind-continuation', async () => {
        const t = await s.turn('root', P.list, {mock: `text:${s.m('M1')}`});
        s.settled(t.wait);
        s.modelCheck('rewind-recall', 'the continuation recalls only M1', t.reply.slice(0, 200),
          t.reply.includes(s.m('M1')) && !['M2', 'M3', 'E2'].some(m => t.reply.includes(s.m(m))));
      });
      await s.step('reopen-readback', async () => {
        const state = await s.d.snapshot('history-final');
        const binding = state.chats.root?.identity?.currentBinding;
        s.check('root-binding', 'the root keeps a provider binding after edit and rewind', binding ? {provider: binding.providerId ?? null} : null, Boolean(binding?.providerTaskId));
      });
    },
  },

  S6: {
    title: 'Models, variants and usage', validation: ['OCE-13'], parity: true, permissionMode: 'full', phases: ['run'],
    steps: ['catalog', 'turn-with-variant', 'switch-variant', 'usage-store (opt-in)'],
    prompts: n => ({v1: `Reply with exactly V1-${n}. Do not use tools.`, v2: `Reply with exactly V2-${n}. Do not use tools.`}),
    unverified: ['`opencode stats --models --cost` comparison (read-only CLI against the isolated profile) is an operator step.',
      'Disconnecting a provider and the accounts card are UI steps outside this runner.'],
    async run(s, P) {
      await s.step('catalog', async () => {
        const harness = s.provider;
        const cached = await s.d.rpc.call('ListModels', {harness});
        const fresh = await s.d.rpc.call('ListModels', {harness, refresh: true});
        s.evidence.save('catalog-cached.json', cached);
        s.evidence.save('catalog-refreshed.json', fresh);
        s.check('catalog-not-empty', 'the catalog lists models right after boot and after refresh (OCF-100)', {cached: cached.length, refreshed: fresh.length}, cached.length > 0 && fresh.length > 0);
        const model = fresh.find(m => m.id === s.config.model) ?? cached.find(m => m.id === s.config.model);
        s.check('selected-model-listed', 'the run model is in the catalog', s.config.model, Boolean(model));
        s.ids.levels = model?.reasoningLevels ?? [];
        s.info('variants-offered', 'the model advertises variants (reasoning levels)', s.ids.levels, s.ids.levels.length > 0);
        s.check('no-plan-mode', 'no catalog entry exposes the native plan agent as a model or mode (OCF-105)', fresh.filter(m => /(^|\/)plan$/i.test(m.id)).map(m => m.id), !fresh.some(m => /(^|\/)plan$/i.test(m.id)));
      });
      await s.step('turn-with-variant', async () => {
        const t = await s.turn('root', P.v1, {mock: `text:${s.m('V1')}`});
        s.settled(t.wait);
        const record = await s.d.chatRecord(s.chatId('root'));
        s.evidence.save('effective-config-1.json', record?.config ?? null);
        s.check('effective-model', 'readback model equals the requested model', record?.config?.model ?? null, record?.config?.model === s.config.model);
        s.check('effective-variant', 'readback variant equals the requested variant', record?.config?.reasoning ?? null, (record?.config?.reasoning ?? null) === (s.config.reasoning ?? null));
        const usage = s.d.chatByRole('root').mirror.latest.contextUsage;
        s.info('context-usage', 'the transcript carries context usage after a turn (context window from the model limit, OCF-159)', usage, Boolean(usage));
      });
      await s.step('switch-variant', async () => {
        const next = (s.ids.levels ?? []).find(level => level !== s.config.reasoning);
        if (!next) return s.info('switch-variant', 'a second variant exists to switch to', s.ids.levels, false);
        const result = await s.d.setModel(s.chatId('root'), {reasoning: next});
        s.check('set-model-applied', 'setModel (next safe turn) applied', {status: result.receipt?.status, reason: result.receipt?.reason?.message ?? null}, result.receipt?.status === 'applied');
        const t = await s.turn('root', P.v2, {mock: `text:${s.m('V2')}`});
        s.settled(t.wait);
        const record = await s.d.chatRecord(s.chatId('root'));
        s.evidence.save('effective-config-2.json', record?.config ?? null);
        s.check('variant-switched', 'readback variant follows the switch (OCF-104)', record?.config?.reasoning ?? null, record?.config?.reasoning === next);
      });
      await s.step('usage-store', async () => {
        if (!s.opts['allow-usage-store']) return s.info('usage-store', 'GetUsageSnapshot skipped without --allow-usage-store (QF-23)', 'skipped', true);
        const snapshot = await s.d.rpc.call('GetUsageSnapshot', {days: 1, includeLimits: false}, {timeoutMs: 120000});
        s.evidence.save('usage-snapshot.json', snapshot);
        s.info('usage-history', 'Usage history returned for the day (compare with the run turns)', Object.keys(snapshot ?? {}), Boolean(snapshot));
      });
    },
  },

  S7: {
    title: 'Images, compaction and discovery', validation: ['OCE-08', 'OCE-14', 'OCE-20'], parity: true, permissionMode: 'full', phases: ['run'],
    steps: ['image-initial', 'image-follow-up', 'image-queued', 'image-steer', 'compaction', 'discovery', 'skill-pick', 'command-with-attachment'],
    prompts: n => ({
      skill: 'Use the selected skill and follow its instructions exactly. Do not use tools.',
      commandImage: '/qa-img',
      image: 'What is the dominant color of the attached image? Reply with exactly one lowercase color word and nothing else. Do not use tools.',
      busy: `Use your shell tool to run exactly this command: sleep 12\nThen reply with exactly SLEPT-${n}.`,
      luna: `Call the get_workspace tool from the luna MCP server. Then reply with exactly WS-${n}- followed by the project name. Do not use any other tool.`,
      command: '/qa-echo',
    }),
    fixture(fx, nonce) {
      const body = `---\ndescription: LUNA QA echo command\n---\nReply with exactly CMD-ECHO-${nonce} and nothing else. Do not use tools.\n`;
      const imageBody = `---\ndescription: LUNA QA image command\n---\nReply with exactly CMD-IMG-${nonce}- followed by the dominant color of the attached image as one lowercase word. Do not use tools.\n`;
      for (const dir of ['.opencode/command', '.claude/commands']) {
        fs.mkdirSync(path.join(fx.dir, dir), {recursive: true});
        fs.writeFileSync(path.join(fx.dir, dir, 'qa-echo.md'), body);
        fs.writeFileSync(path.join(fx.dir, dir, 'qa-img.md'), imageBody);
      }
      // One project skill both providers discover (.claude/skills, QF-02).
      fs.mkdirSync(path.join(fx.dir, '.claude/skills/qa-skill'), {recursive: true});
      fs.writeFileSync(path.join(fx.dir, '.claude/skills/qa-skill/SKILL.md'),
        `---\nname: qa-skill\ndescription: LUNA QA skill that answers with a fixed marker\n---\nWhen this skill is used, reply with exactly SKILL-OK-${nonce} and nothing else.\n`);
      fx.git('add', '.opencode', '.claude');
      fx.git('commit', '-q', '-m', 'qa commands and skill');
    },
    unverified: ['Auto-compaction mid-turn (OCE-14) needs a small-context mock; this scenario runs manual compaction.',
      'Invalid, oversized and SVG attachments and the app-restart image path (OCE-08 5-6) are not driven here.'],
    async run(s, P) {
      const colors = {red: [220, 20, 20], blue: [20, 40, 220], green: [20, 200, 40], yellow: [235, 210, 20], purple: [130, 30, 200]};
      const image = async name => s.d.upload(solidPng(64, 64, colors[name]), `qa-${name}.png`);
      const busyTurn = async () => {
        const sent = await s.d.send(s.chatId('root'), s.p(P.busy, 'tool-shell:sleep 12'));
        await waitOn([s.d.sessions, s.d.chatByRole('root').mirror], () => s.d.busy(s.chatId('root')), 60000, 'busy').catch(() => null);
        return sent;
      };
      await s.step('image-initial', async () => {
        const staged = await image('red');
        const t = await s.turn('root', P.image, {mock: 'image', attachments: [staged.path]});
        s.settled(t.wait);
        const user = s.entries('root').find(e => e.id === t.messageId);
        s.info('image-part', 'image parts on the user row (LUNA keeps staged images out of the mirrored parts for every provider; the wire files[] check is decisive)', (user?.parts ?? []).map(p => p.kind), (user?.parts ?? []).some(p => p.kind === 'image'));
        s.modelCheck('image-consumed', 'the model names the image colour (red)', t.reply.slice(0, 80), /\bred\b/i.test(t.reply));
        s.ids.images = [{step: 'image-initial', ...staged}];
      });
      await s.step('image-follow-up', async () => {
        const staged = await image('yellow');
        const t = await s.turn('root', P.image, {mock: 'image', attachments: [staged.path]});
        s.settled(t.wait);
        s.check('follow-up-path', 'an idle follow-up with an image is sent as a run', t.path, t.path === 'run');
        const user = s.entries('root').find(e => e.id === t.messageId);
        s.info('follow-up-image-part', 'the follow-up user row shows the image part', (user?.parts ?? []).map(p => p.kind), (user?.parts ?? []).some(p => p.kind === 'image'));
        s.modelCheck('follow-up-image-consumed', 'the model names the follow-up image colour (yellow)', t.reply.slice(0, 80), /\byellow\b/i.test(t.reply));
        s.ids.images.push({step: 'image-follow-up', ...staged});
      });
      await s.step('image-queued', async () => {
        const busy = await busyTurn();
        const staged = await image('blue');
        const queued = await s.d.send(s.chatId('root'), s.p(P.image, 'image'), {attachments: [staged.path], queue: true});
        s.check('queued-path', 'a busy chat queues the image row (queueMessage)', queued.path, queued.path === 'queueMessage');
        const queue = s.d.chatByRole('root').queue;
        s.info('queued-row-attachment', 'the queue row keeps its attachment', (queue.latest?.items ?? []).filter(i => i.id === queued.messageId).map(i => (i.attachments ?? []).length), (queue.latest?.items ?? []).some(i => i.id === queued.messageId && (i.attachments ?? []).length === 1));
        await s.d.waitTurn(s.chatId('root'), {mark: busy.mark, messageId: busy.messageId, timeoutMs: s.turnTimeout, answer: allowAll});
        const delivered = await s.d.chatByRole('root').mirror.waitFor(() => s.d.replySettled(s.entries('root'), queued.messageId), {timeoutMs: s.turnTimeout, label: 'queued image delivered'}).catch(() => false);
        s.check('queued-delivered', 'the queued image row is delivered at idle as its own turn', delivered, Boolean(delivered));
        const reply = s.replyAfter('root', queued.messageId);
        s.modelCheck('queued-image-consumed', 'the model names the queued image colour (blue)', reply.slice(0, 80), /\bblue\b/i.test(reply));
        s.ids.images.push({step: 'image-queued', ...staged});
      });
      await s.step('image-steer', async () => {
        const busy = await busyTurn();
        const staged = await image('green');
        const queued = await s.d.send(s.chatId('root'), s.p(P.image, 'image'), {attachments: [staged.path], queue: true});
        const steer = await s.d.steerQueued(s.chatId('root'), queued.messageId);
        s.check('steer-receipt', 'steerQueuedNow is applied or truthfully refused', {status: steer.receipt?.status, reason: steer.receipt?.reason?.code ?? null}, ['applied', 'rejected', 'conflict'].includes(steer.receipt?.status));
        await s.d.waitTurn(s.chatId('root'), {mark: busy.mark, messageId: busy.messageId, timeoutMs: s.turnTimeout, answer: allowAll});
        // Wait for the steered row's own reply (steered into the live turn or
        // delivered as the next turn), not merely for the row to appear.
        await s.d.chatByRole('root').mirror.waitFor(() => s.d.replySettled(s.entries('root'), queued.messageId), {timeoutMs: s.turnTimeout, label: 'steer delivered'}).catch(() => null);
        await s.d.waitIdle(s.chatId('root'), {timeoutMs: s.turnTimeout}).catch(() => null);
        const count = s.entries('root').filter(e => e.id === queued.messageId).length;
        s.check('steer-once', 'the steered image message appears exactly once (OCF-32/33)', count, count === 1);
        const reply = s.replyAfter('root', queued.messageId) || s.entries('root').filter(e => e.role === 'assistant').slice(-2).map(textOf).join('\n');
        s.modelCheck('steer-image-consumed', 'the model names the steered image colour (green)', reply.slice(-120), /\bgreen\b/i.test(reply));
        s.ids.images.push({step: 'image-steer', ...staged});
      });
      await s.step('compaction', async () => {
        await s.d.waitIdle(s.chatId('root'), {timeoutMs: s.turnTimeout}).catch(() => null);
        const mirror = s.d.chatByRole('root').mirror;
        const before = JSON.stringify(mirror.latest.contextControl ?? null);
        const compact = await s.d.compact(s.chatId('root'));
        s.check('compact-receipt', 'compactContext applied (or truthfully refused)', {status: compact.receipt?.status, reason: compact.receipt?.reason?.code ?? null}, ['applied', 'rejected'].includes(compact.receipt?.status));
        if (compact.receipt?.status === 'applied') {
          await mirror.waitFor(() => JSON.stringify(mirror.latest.contextControl ?? null) !== before, {timeoutMs: 120000, label: 'compaction lifecycle'}).catch(() => null);
        }
        s.info('compaction-lifecycle', 'compaction shows as context-control lifecycle, not a turn end (OCF-110/156)', mirror.latest.contextControl, JSON.stringify(mirror.latest.contextControl ?? null) !== before);
        const t = await s.turn('root', P.luna, {mock: 'text', answer: allowAll});
        s.settled(t.wait);
        const calls = lunaParts(s.after('root', t.messageId));
        s.modelCheck('luna-after-compaction', 'LUNA tools survive compaction (OCF-111)', calls.map(p => `${p.resolved}:${p.isError}`), calls.some(p => p.resolved && !p.isError));
      });
      await s.step('discovery', async () => {
        const commands = await s.d.rpc.call('ListCommands', {harness: s.provider, chatId: s.chatId('root')}, {timeoutMs: 90000}).catch(error => ({error: String(error.message ?? error)}));
        s.evidence.save('commands.json', commands);
        const list = Array.isArray(commands) ? commands : commands?.commands ?? [];
        s.check('project-command-listed', 'the project-only command qa-echo is discovered for this chat (OCF-04)', list.map(c => c.name ?? c.id ?? c).filter(n => /qa-echo/.test(String(n))), list.some(c => /qa-echo/.test(String(c.name ?? c.id ?? c))));
        const catalog = await s.d.rpc.call('ListComposerCatalog', {chatId: s.chatId('root')}, {timeoutMs: 90000}).catch(error => ({error: String(error.message ?? error)}));
        s.evidence.save('composer-catalog.json', catalog);
        const t = await s.turn('root', P.command, {mock: `text:CMD-ECHO-${s.nonce}`, answer: allowAll});
        s.settled(t.wait);
        s.modelCheck('command-ran', `the slash command ran (CMD-ECHO-${s.nonce})`, t.reply.slice(0, 120), t.reply.includes(`CMD-ECHO-${s.nonce}`));
        const record = await s.d.chatRecord(s.chatId('root'));
        s.info('title-generated', 'the chat has a generated title (run_title)', record?.title ?? null, Boolean(record?.title));
      });
      await s.step('skill-pick', async () => {
        // ListComposerCatalog is the Codex native-integrations catalog and
        // refuses other harnesses; Claude and OpenCode list skills through
        // ListCommands (kind skill), which the composer offers under `$`.
        const commands = await s.d.rpc.call('ListCommands', {harness: s.provider, chatId: s.chatId('root')}, {timeoutMs: 90000}).catch(error => ({error: String(error.message ?? error)}));
        s.evidence.save('commands-skills.json', commands);
        const list = Array.isArray(commands) ? commands : commands?.commands ?? [];
        const entry = list.find(e => e.kind === 'skill' && /qa-skill/.test(e.name));
        s.check('skill-listed', 'the project skill qa-skill is listed as a skill for this chat (ListCommands kind skill)', entry ? {kind: entry.kind, name: entry.name} : list.filter(e => e.kind === 'skill').map(e => e.name), Boolean(entry));
        if (!entry) return;
        // (a) The composer path: accept_slash writes `$qa-skill` as text (Claude's
        // wire text becomes `/qa-skill`); no typed input item is attached.
        const a = await s.turn('root', `$${entry.name} ${P.skill}`, {mock: `text:SKILL-OK-${s.nonce}`, answer: allowAll});
        s.settled(a.wait, 'skill-text-settled');
        s.modelCheck('skill-text-used', `a $${entry.name} pick sent as composer text reached the skill (SKILL-OK-${s.nonce})`, a.reply.slice(0, 120), a.reply.includes(`SKILL-OK-${s.nonce}`));
        // (b) The typed selection the adapters accept (ComposerInput::Skill).
        // ListCommands may omit a skill path. This scenario owns the exact
        // project fixture, so use its real file rather than an empty path.
        const skillPath = entry.path || path.join(s.fixture.dir, '.claude/skills/qa-skill/SKILL.md');
        assert.ok(path.isAbsolute(skillPath) && fs.statSync(skillPath).isFile(), 'typed skill needs the existing absolute fixture path');
        const t = await s.turn('root', P.skill, {mock: `text:SKILL-OK-${s.nonce}`, inputItems: [{type: 'skill', name: entry.name, path: skillPath}], answer: allowAll});
        s.settled(t.wait);
        const user = s.entries('root').find(e => e.id === t.messageId);
        s.info('skill-row', 'the user row records the typed skill selection', (user?.parts ?? []).map(p => p.kind), true);
        s.modelCheck('skill-used', `the typed skill pick reached the model (SKILL-OK-${s.nonce})`, t.reply.slice(0, 120), t.reply.includes(`SKILL-OK-${s.nonce}`));
      });
      await s.step('command-with-attachment', async () => {
        const staged = await image('purple');
        const t = await s.turn('root', P.commandImage, {mock: 'image', attachments: [staged.path], answer: allowAll});
        s.settled(t.wait);
        const user = s.entries('root').find(e => e.id === t.messageId);
        s.info('command-image-part', 'the slash-command row keeps its image part', (user?.parts ?? []).map(p => p.kind), (user?.parts ?? []).some(p => p.kind === 'image'));
        s.modelCheck('command-ran-with-image', `the command ran (CMD-IMG-${s.nonce}) and saw the image (purple/violet)`, t.reply.slice(0, 120), t.reply.includes(`CMD-IMG-${s.nonce}`) && /purple|violet|magenta/i.test(t.reply));
        s.ids.images.push({step: 'command-with-attachment', ...staged});
      });
    },
  },

  S8: {
    title: 'Review and goals', validation: ['OCE-23'], parity: true, permissionMode: 'full', phases: ['run'],
    steps: ['review-uncommitted', 'review-commit', 'review-base-branch', 'goals-unavailable'],
    prompts: () => ({}),
    fixture(fx) {
      fx.git('branch', 'qa-base');
      fs.writeFileSync(path.join(fx.dir, 'calc.js'), 'export const add = (a, b) => a - b; // deliberate bug for review\n');
      fx.git('add', 'calc.js');
      fx.git('commit', '-q', '-m', 'Add calc helper');
      fx.sha = fx.git('rev-parse', 'HEAD');
      fs.appendFileSync(path.join(fx.dir, 'calc.js'), 'export const mul = (a, b) => a + b;\n');
    },
    unverified: ['The provider-side review command body (OpenCode /review, Claude /code-review) is in the wire log (OpenCode) or the Claude transcript; not asserted here.'],
    async run(s) {
      // The review row id is engine-owned, so the turn settles on activity
      // after the mark and the new rows are read by position.
      const review = async (label, target) => s.step(label, async () => {
        await s.d.waitIdle(s.chatId('root')).catch(() => null);
        const before = s.entries('root').length;
        const started = await s.d.review(s.chatId('root'), target);
        s.check('review-receipt', `nativeReview for ${target.type} applied`, {status: started.receipt?.status, reason: started.receipt?.reason?.message ?? null}, started.receipt?.status === 'applied');
        if (started.receipt?.status !== 'applied') return;
        const wait = await s.d.waitTurn(s.chatId('root'), {mark: started.mark, timeoutMs: s.turnTimeout * 2, answer: allowAll});
        s.settled(wait, 'review-settled', ['idle']);
        const added = s.entries('root').slice(before);
        const user = added.find(e => e.role === 'user');
        s.info('review-row', 'the review row carries the shared review label', user ? textOf(user).slice(0, 120) : null, Boolean(user) && textOf(user).includes(reviewLabel(target).split('\n')[0]));
        const reply = added.filter(e => e.role === 'assistant').map(textOf).join('\n').trim();
        s.info('review-output', 'the review produced output', reply.length, reply.length > 0);
      });
      await review('review-uncommitted', {type: 'uncommittedChanges'});
      await review('review-commit', {type: 'commit', sha: s.fixture.sha, title: 'Add calc helper'});
      await review('review-base-branch', {type: 'baseBranch', branch: 'qa-base'});
      await s.step('goals-unavailable', async () => {
        const goal = await s.d.goal(s.chatId('root'), {action: 'get'});
        s.info('goal-get', 'goals: OpenCode reports unavailable with the shared copy; Claude has native /goal', {status: goal.receipt?.status, reason: goal.receipt?.reason?.message ?? null}, true);
      });
    },
  },

  // ---------------------------------------------------------------- live-only extras
  // Added for the in-app E2E round: background child asks after the root Done,
  // server faults, a provider stall and automatic compaction. They are `extra`:
  // never rehearsed (the fake engine has no background children, server
  // processes or mock provider) and outside the S1..S8 parity set.
  S2X: {
    title: 'Background child ask after Done, wake turn, parked follow-up, foreground child Stop', validation: ['OCE-01', 'OCE-05', 'OCE-21'],
    parity: true, extra: true, permissionMode: 'ask', phases: ['run'],
    steps: ['late-child-ask', 'follow-up-while-parked', 'foreground-child-stop'],
    prompts: n => ({
      late: `Use your subagent tool with background set to true (a background subagent) to launch ONE subagent. ${SHELL_AGENT} Give it the description "Asking child" and this task: "First use your file read tool to read README.md, then use your file search (glob) tool to list the *.md files, then use your shell tool to run exactly this command and nothing else: python3 -c 'print("LATE-ASK-${n}")'. Then reply with only the command output." Do not wait for it and do not run the command yourself: immediately reply with exactly ROOT-DONE-${n} and end your turn.`,
      parked: `Use your subagent tool with background set to true (a background subagent) to launch ONE subagent. ${SHELL_AGENT} Give it the description "Sleeping child" and this task: "Use your shell tool to run exactly this command and nothing else: python3 -c 'import time; time.sleep(45); print("PARKED-CHILD-${n}")'. Then reply with only the command output." Do not wait for it and do not run the command yourself: immediately reply with exactly PARKED-ROOT-${n} and end your turn.`,
      followup: `Reply with exactly FOLLOWUP-${n}. Do not use tools.`,
      fg: `Use your subagent tool to launch ONE subagent in the foreground (do not run it in the background). ${SHELL_AGENT} Give it the description "Foreground child" and this task: "Use your shell tool to run exactly this command and nothing else: python3 -c 'import time; time.sleep(90); print("FG-CHILD-${n}")'. Then reply with only the command output." Wait for the subagent to finish, then reply with exactly FG-ROOT-${n} followed by what the subagent reported.`,
    }),
    unverified: ['Whether the child asks after the root Done depends on model timing; an ask during the root turn is answered and recorded as early (ask-timing).',
      'Claude Code background agents may not raise host asks; the Claude run records what it does.'],
    async run(s, P) {
      const chatId = s.chatId('root');
      const wakeAfter = (entries, anchorId) => {
        const at = entries.findIndex(e => e.id === anchorId);
        return at < 0 ? [] : entries.slice(at + 1).filter(e => e.role === 'assistant' && ['complete', 'aborted'].includes(e.status) && (e.parts ?? []).some(p => p.kind === 'text' || p.kind === 'reasoning'));
      };
      await s.step('late-child-ask', async () => {
        const early = [];
        const t = await s.turn('root', P.late, {mock: 'subagent-background', answer: pending => {
          if (!pending.questions.some(q => q.approval)) return null;
          early.push({requestId: pending.requestId});
          return {approval: 'allow-once'};
        }});
        s.settled(t.wait, 'root-turn-settled');
        s.modelCheck('root-done', `root replied ROOT-DONE-${s.nonce}`, t.reply.slice(0, 160), t.reply.includes(s.m('ROOT-DONE')));
        s.ids.rootDone = s.after('root', t.messageId).filter(e => e.role === 'assistant').at(-1)?.id ?? null;
        s.info('ask-timing', 'the child asked after the root Done (late) rather than during the root turn', {early: early.length}, early.length === 0);
        const chat = s.d.chatByRole('root');
        if (!early.length) {
          const late = await chat.mirror.waitFor(() => chat.mirror.currentPending(), {timeoutMs: 150000, label: 'late child ask'}).catch(() => null);
          await s.d.holdForCapture('s2x-late-ask-card');
          s.check('late-ask-shown', 'the background child approval reaches the root chat panel after the root Done (OCF-51)',
            late ? late.questions.map(q => ({header: q.header ?? null, kind: q.approval?.actionKind ?? 'question', subagent: q.approval?.details?.subagent ?? null})) : null,
            Boolean(late?.questions.some(q => q.approval)));
          if (late?.questions.some(q => q.approval)) {
            const {receipt} = await s.d.answer(chatId, late, {approval: 'allow-once'});
            s.check('late-ask-answered', 'Allow once on the late card is applied', receipt?.status, receipt?.status === 'applied');
          }
        }
        const wake = await waitAnswering(s, 'root', entries => (wakeAfter(entries, s.ids.rootDone).length && !s.d.busy(chatId) ? wakeAfter(entries, s.ids.rootDone) : null),
          {timeoutMs: 240000, label: 'wake turn after the late ask'});
        const wakeEntries = wake.value ?? wakeAfter(s.entries('root'), s.ids.rootDone);
        s.check('wake-turn-rendered', 'the background completion renders as its own assistant (wake) turn after ROOT-DONE', wakeEntries.map(e => ({status: e.status, parts: (e.parts ?? []).map(p => p.kind)})), wakeEntries.length >= 1);
        s.info('wake-reasoning', 'the wake turn keeps its reasoning parts', wakeEntries.map(e => (e.parts ?? []).filter(p => p.kind === 'reasoning').length), wakeEntries.some(e => (e.parts ?? []).some(p => p.kind === 'reasoning')));
        const children = await attachChildren(s, 'root', 'late-child', t.messageId);
        const childEntries = children.flatMap(role => s.entries(role));
        s.modelCheck('late-child-ran', `the child ran its command (LATE-ASK-${s.nonce} in the child transcript)`, markerInTools(allTools(childEntries), s.m('LATE-ASK')), markerInTools(allTools(childEntries), s.m('LATE-ASK')));
        const declined = AUTO_DECLINED.test(textDump([...s.entries('root'), ...childEntries]));
        s.check('no-auto-decline', 'no approval was closed without an answer (root and child documents)', declined, !declined);
        s.check('no-open-card', 'no unresolved card is left on the root after the wake', chat.mirror.pendingInputs().length, chat.mirror.pendingInputs().length === 0);
        s.d.capture('s2x-wake-turn');
      });
      await s.step('follow-up-while-parked', async () => {
        await s.d.waitIdle(chatId, {timeoutMs: 180000}).catch(() => null);
        const t = await s.turn('root', P.parked, {mock: 'subagent-background', answer: allowAll});
        s.settled(t.wait, 'parked-root-settled');
        s.modelCheck('parked-root-reply', `root replied PARKED-ROOT-${s.nonce}`, t.reply.slice(0, 160), t.reply.includes(s.m('PARKED-ROOT')));
        const parkedDone = s.after('root', t.messageId).filter(e => e.role === 'assistant').at(-1)?.id ?? null;
        const approved = t.wait.handled.length ? {handled: t.wait.handled}
          : await waitAnswering(s, 'root', (entries, handled) => handled.length >= 1, {timeoutMs: 150000, label: 'sleeping child approval'});
        s.check('parked-child-approved', 'the sleeping child approval was shown and answered (during or after the root turn)', (approved.handled ?? []).map(h => h.status), (approved.handled ?? []).some(h => h.status === 'applied'));
        // Let the status settle after the answer before reading the parked state.
        await sleep(5000);
        const chip = subagentParts(s.after('root', t.messageId))[0];
        s.info('parked-state', 'session status and child chip while parked (root Done, background child running)', {session: s.d.sessions.status(chatId), chip: chip?.subagentStatus ?? null}, chip?.subagentStatus === 'running');
        // Sent at once, like a user typing while the child still runs (no wait for idle).
        const sentF = await s.d.send(chatId, s.p(P.followup, `text:${s.m('FOLLOWUP')}`));
        s.info('follow-up-path', 'how the composer sends while parked (run when idle, queueMessage when busy)', sentF.path, true);
        const fw = await s.d.waitTurn(chatId, {mark: sentF.mark, messageId: sentF.messageId, answer: allowAll, timeoutMs: s.turnTimeout});
        s.settled(fw, 'follow-up-settled');
        const freply = s.replyAfter('root', sentF.messageId);
        s.modelCheck('follow-up-reply', `follow-up replied FOLLOWUP-${s.nonce}`, freply.slice(0, 160), freply.includes(s.m('FOLLOWUP')));
        const count = s.entries('root').filter(e => e.id === sentF.messageId).length;
        s.check('follow-up-once', 'the follow-up user row appears exactly once', count, count === 1);
        const settled = await waitAnswering(s, 'root', entries => {
          const chips = subagentParts(entries.slice(Math.max(0, entries.findIndex(e => e.id === t.messageId))));
          return chips.length && chips.every(p => SUBAGENT_TERMINAL.has(p.subagentStatus)) && !s.d.busy(chatId) ? chips : null;
        }, {timeoutMs: 240000, label: 'parked child settled'});
        s.check('parked-child-settled', 'the parked child chip settles after the follow-up (not lost)', (settled.value ?? []).map(p => p.subagentStatus), Boolean(settled.value));
        const children = await attachChildren(s, 'root', 'parked-child', t.messageId);
        const childEntries = children.flatMap(role => s.entries(role));
        s.modelCheck('parked-child-ran', `PARKED-CHILD-${s.nonce} in the child transcript`, markerInTools(allTools(childEntries), s.m('PARKED-CHILD')), markerInTools(allTools(childEntries), s.m('PARKED-CHILD')));
        s.info('parked-wake', 'assistant turns after the parked root reply (follow-up reply and wake)', wakeAfter(s.entries('root'), parkedDone).map(e => (e.parts ?? []).map(p => p.kind)), true);
        const declined = AUTO_DECLINED.test(textDump([...s.entries('root'), ...childEntries]));
        s.check('no-auto-decline', 'no approval was closed without an answer', declined, !declined);
        s.d.capture('s2x-after-parked');
      });
      await s.step('foreground-child-stop', async () => {
        await s.d.waitIdle(chatId, {timeoutMs: 180000}).catch(() => null);
        const sent = await s.d.send(chatId, s.p(P.fg, 'subagent'));
        const ready = await waitAnswering(s, 'root', (entries, handled) => {
          const at = entries.findIndex(e => e.id === sent.messageId);
          const chip = at >= 0 ? subagentParts(entries.slice(at + 1)).find(p => p.subagentStatus === 'running') : null;
          return chip && handled.length >= 1 ? chip : null;
        }, {timeoutMs: 180000, label: 'foreground child running its approved command'});
        s.check('fg-child-running', 'a running foreground child whose command was approved', ready.value ? {status: ready.value.subagentStatus, approvals: ready.handled.map(h => h.status)} : {timeout: Boolean(ready.timeout)}, Boolean(ready.value));
        if (!ready.value) return;
        await sleep(3000);
        const children = await attachChildren(s, 'root', 'fg-child', sent.messageId);
        s.check('nested-transcript', 'the foreground child has its own nested transcript with entries', children.map(role => s.entries(role).length), children.length >= 1 && children.every(role => s.entries(role).length > 0));
        s.d.capture('s2x-foreground-child');
        const work = await s.d.rpc.call('GetTaskWork', {taskId: chatId});
        s.evidence.save('task-work-before-stop.json', work);
        const item = (work.items ?? []).find(i => i.kind === 'nativeChild' && !['completed', 'failed', 'stopped'].includes(i.state) && i.canStop);
        s.check('child-work-row', 'the running child is a stoppable nativeChild row in the task work panel', item ? {kind: item.kind, state: item.state, canStop: item.canStop} : (work.items ?? []).map(i => ({kind: i.kind, state: i.state, canStop: i.canStop})), Boolean(item));
        if (!item) return;
        const preview = await s.d.rpc.call('PreviewStopTaskWork', {taskId: chatId, workIds: [item.id]});
        s.evidence.save('task-work-stop-preview.json', preview);
        const requestId = crypto.randomUUID();
        const send = recordSend(s.evidence, {kind: 'stopWork', label: 'per-child stop', step: 'foreground-child-stop', taskId: chatId, workIds: [item.id], previewId: preview.previewId, requestId});
        let receipt;
        try {
          receipt = await s.d.rpc.call('StopTaskWork', {taskId: chatId, workIds: [item.id], previewId: preview.previewId, requestId});
          settleSend(s.evidence, send, {outcome: 'applied', receipt});
        } catch (error) {
          settleSend(s.evidence, send, {outcome: error.unknown ? 'unknown' : 'rejected', error: String(error.message ?? error)});
          throw error;
        }
        s.evidence.save('task-work-stop-receipt.json', receipt);
        s.check('child-stop-receipt', 'StopTaskWork reports the child stopped or stop requested', (receipt?.results ?? []).map(r => r.status), (receipt?.results ?? []).some(r => ['stopped', 'requested'].includes(r.status)));
        const chipEnd = await waitAnswering(s, 'root', entries => {
          const at = entries.findIndex(e => e.id === sent.messageId);
          const chip = at >= 0 ? subagentParts(entries.slice(at + 1))[0] : null;
          return chip && SUBAGENT_TERMINAL.has(chip.subagentStatus) ? chip : null;
        }, {timeoutMs: 90000, label: 'child chip terminal after per-child stop'});
        s.check('child-chip-stopped', 'the child chip ends terminal after the per-child Stop', chipEnd.value?.subagentStatus ?? null, Boolean(chipEnd.value));
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, answer: allowAll, timeoutMs: 180000});
        s.settled(wait, 'root-turn-ends-after-child-stop', ['idle', 'errored']);
        s.info('root-after-child-stop', 'the root turn continues and ends on its own after only the child was stopped', {status: wait.status, statuses: wait.statuses, reply: s.replyAfter('root', sent.messageId).slice(0, 200)}, wait.status === 'idle');
        const leaked = markerInTools(allTools(children.flatMap(role => s.entries(role))), s.m('FG-CHILD'));
        s.check('child-command-stopped', `FG-CHILD-${s.nonce} never appears after the per-child Stop`, leaked, !leaked);
        s.d.capture('s2x-after-child-stop');
      });
    },
  },

  S4X: {
    title: 'OpenCode server paused (SIGSTOP/SIGCONT) or killed (SIGKILL) with an approval or a form pending', validation: ['OCE-04', 'OCE-05'],
    parity: false, extra: true, permissionMode: 'ask', phases: ['run'],
    steps: ['sigstop-approval', 'kill-approval', 'continue-after-kill', 'sigstop-form', 'kill-form', 'continue-after-form-kill'],
    prompts: n => ({
      approval: tag => `Use your shell tool to run exactly this command: echo S4X-${tag}-${n}\nThen reply with only the command's output. Do not run any other command.`,
      form: tag => `${BLOCKING_QUESTION} Use your tool for asking the user a multiple-choice question (do not ask in plain text). Ask me to choose exactly one color from these options: red, green, blue. After I answer, reply with exactly S4X-${tag}-${n}-<the color I chose>.`,
      after: tag => `Reply with exactly S4X-${tag}-${n}. Do not use tools.`,
    }),
    unverified: ['Only the chat\'s OpenCode server process is faulted; LUNA is not restarted (S4 covers an app restart).',
      'SIGSTOP pauses the whole server (SSE and HTTP); a pure SSE cut needs a proxy (the adapter has a recorded steer-drop probe).'],
    async run(s, P) {
      const chatId = s.chatId('root');
      const holdMs = Number(s.opts['hold-ms'] ?? 75000);
      const lower = tag => tag.toLowerCase();
      // Short-lived catalog servers can share the chat's cwd: wait for one
      // match, else take the longest-running server and record the choice.
      const server = async () => {
        let found = [];
        for (let i = 0; i < 10; i += 1) {
          found = opencodeServersFor(s.d.identity.pid, s.fixture.dir);
          if (found.length === 1) return found[0];
          await sleep(500);
        }
        assert.ok(found.length > 0, 'no opencode server child runs for the fixture');
        const age = pid => {
          const raw = spawnSync('ps', ['-p', String(pid), '-o', 'etimes='], {encoding: 'utf8'}).stdout.trim();
          return Number(raw) || 0;
        };
        const pick = [...found].sort((a, b) => age(b.pid) - age(a.pid))[0];
        s.info('server-choice', 'several opencode servers share the fixture cwd; the longest-running one was faulted', found.map(f => ({pid: f.pid, age: age(f.pid)})), true);
        return pick;
      };
      const begin = async (tag, kind) => {
        await s.d.waitIdle(chatId, {timeoutMs: 120000}).catch(() => null);
        const text = kind === 'approval' ? P.approval(tag) : P.form(tag);
        if (s.tier === 'T2' && kind === 'form') {
          // The mock has no question scenario: queue a scripted `question`
          // tool call (the prompt carries no marker, so the script is used).
          assert.ok(s.opts['mock-url'], 'S4X forms at T2 need --mock-url to script the question tool');
          const entry = {scenario: 'tool', name: 'question', arguments: {questions: [{question: 'Choose exactly one color', header: 'Color',
            options: [{label: 'red', description: 'red'}, {label: 'green', description: 'green'}, {label: 'blue', description: 'blue'}], multiple: false}]}};
          const res = await fetch(`${s.opts['mock-url']}/__mock/script`, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(entry)});
          s.evidence.command({method: 'mock-script', params: entry, outcome: res.status, phase: s.evidence.phase, at: now()});
          assert.equal(res.status, 200, 'the mock refused the scripted question entry');
        }
        const sent = await s.d.send(chatId, s.p(text, kind === 'approval' ? `tool-shell:echo S4X-${tag}-${s.nonce}` : null));
        const pending = await s.d.waitPending(chatId, {timeoutMs: 180000});
        const isApproval = pending.questions.some(q => q.approval);
        s.check(`${lower(tag)}-pending`, `a pending ${kind} before the fault`, {approval: isApproval, questions: pending.questions.length}, kind === 'approval' ? isApproval : !isApproval);
        s.d.capture(`s4x-${lower(tag)}-before`);
        return {sent, pending};
      };
      const answerFor = kind => (kind === 'approval' ? {approval: 'allow-once'} : {pick: 'green'});
      const outcome = (tag, kind, sent) => {
        const reply = s.replyAfter('root', sent.messageId);
        if (kind === 'approval') {
          const hits = s.toolsAfter('root', sent.messageId).filter(t => !t.isError && String(t.output ?? '').includes(s.m(`S4X-${tag}`))).length;
          s.modelCheck(`${lower(tag)}-output`, `the approved command ran (S4X-${tag}-${s.nonce})`, {hits, reply: reply.slice(0, 120)}, hits >= 1 || reply.includes(s.m(`S4X-${tag}`)));
          s.check(`${lower(tag)}-ran-at-most-once`, 'the approved command ran at most once (no replay)', hits, hits <= 1);
        } else {
          s.modelCheck(`${lower(tag)}-form-consumed`, `reply is S4X-${tag}-${s.nonce}-green`, reply.slice(0, 160), reply.toLowerCase().includes(`s4x-${tag}-${s.nonce}-green`.toLowerCase()));
        }
      };
      const sigstop = async (tag, kind) => {
        const {sent, pending} = await begin(tag, kind);
        const srv = await server();
        s.evidence.save(`s4x-${lower(tag)}-server.json`, srv);
        let held;
        signalServer(s, srv, 'SIGSTOP');
        try {
          held = await timeline(s, 'root', holdMs, 5000);
        } finally {
          signalServer(s, srv, 'SIGCONT');
        }
        const resumed = await timeline(s, 'root', 45000, 3000);
        s.evidence.save(`s4x-${lower(tag)}-timeline.json`, {pendingBefore: pending.requestId, holdMs, held, resumed});
        const cards = s.d.chatByRole('root').mirror.pendingInputs();
        s.check(`${lower(tag)}-one-open-card`, 'after SIGCONT exactly one open card for the request (re-shown once, no duplicates)', cards.map(c => (c.requestId === pending.requestId ? 'same-request' : 'other-request')), cards.length === 1);
        s.d.capture(`s4x-${lower(tag)}-after-resume`);
        const live = s.d.chatByRole('root').mirror.currentPending();
        if (live) {
          const {receipt} = await s.d.answer(chatId, live, answerFor(kind));
          s.check(`${lower(tag)}-answerable`, 'the pending request is answerable after the pause (receipt applied)', receipt?.status, receipt?.status === 'applied');
        } else {
          s.check(`${lower(tag)}-answerable`, 'the pending request is answerable after the pause', 'no open card', false);
        }
        // The answered card stays open in the mirror until the provider's
        // reply lands; ignore it so the wait does not end on it as unanswered.
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, answer: allowAll, ignore: live ? [live.requestId] : [], timeoutMs: s.turnTimeout});
        s.settled(wait, `${lower(tag)}-turn-completes`);
        outcome(tag, kind, sent);
        const inputs = s.after('root', sent.messageId).flatMap(e => (e.parts ?? []).filter(p => p.kind === 'input'));
        s.info(`${lower(tag)}-input-parts`, 'input parts written for this turn (resolved flags)', inputs.map(p => p.resolved), inputs.length <= 2);
      };
      const kill = async (tag, kind) => {
        const {sent, pending} = await begin(tag, kind);
        const srv = await server();
        s.evidence.save(`s4x-${lower(tag)}-server.json`, srv);
        signalServer(s, srv, 'SIGKILL');
        const killedAt = now();
        const mirror = s.d.chatByRole('root').mirror;
        const after = await timeline(s, 'root', 150000, 3000, () => !s.d.busy(chatId) || mirror.pendingInputs().some(p => p.requestId !== pending.requestId));
        s.evidence.save(`s4x-${lower(tag)}-timeline.json`, {pendingBefore: pending.requestId, killedAt, after});
        s.d.capture(`s4x-${lower(tag)}-after-kill`);
        const cards = mirror.pendingInputs();
        s.check(`${lower(tag)}-no-duplicate-cards`, 'at most one open card after the kill', cards.length, cards.length <= 1);
        s.info(`${lower(tag)}-user-view`, 'what the user sees after the server was killed', userView(s, 'root'), true);
        const reshown = cards.find(c => c.requestId !== pending.requestId) ?? null;
        if (reshown) {
          const {receipt} = await s.d.answer(chatId, mirror.currentPending() ?? reshown, answerFor(kind));
          s.check(`${lower(tag)}-reshown-answerable`, 'a re-shown request after the kill is answerable', receipt?.status, receipt?.status === 'applied');
          const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, answer: allowAll, timeoutMs: s.turnTimeout});
          s.settled(wait, `${lower(tag)}-turn-completes`, ['idle', 'errored']);
          outcome(tag, kind, sent);
          return;
        }
        const busy = s.d.busy(chatId);
        s.check(`${lower(tag)}-not-stuck`, 'the chat is not left working after the server died (ends within 150 s)', s.d.sessions.status(chatId), !busy);
        const stillOpen = cards.some(c => c.requestId === pending.requestId);
        s.check(`${lower(tag)}-old-card-closed`, 'the pre-kill card is closed or truthfully kept (not left open as if live)', {stillOpen, resolved: mirror.inputResolved(pending.requestId)}, !stillOpen || busy);
        let probe;
        try {
          probe = (await s.d.answer(chatId, pending, answerFor(kind), {probe: true})).receipt;
        } catch (error) {
          probe = {status: error instanceof NotAnswerable ? 'not-answerable' : 'rpc-error', reason: {message: String(error.message ?? error)}};
        }
        s.info(`${lower(tag)}-stale-probe`, 'answering the pre-kill request has no effect (receipt recorded)', {status: probe?.status, reason: probe?.reason?.code ?? probe?.reason?.message ?? null}, probe?.status !== 'applied');
        if (kind === 'approval') {
          const quiet = await s.stays('root', entries => markerInTools(allTools(entries), s.m(`S4X-${tag}`)), 15000);
          s.check(`${lower(tag)}-no-late-run`, 'the killed request never runs afterwards', quiet, quiet);
        }
        const last = mirror.entries.filter(e => e.role === 'assistant').at(-1);
        const lastText = textOf(last ?? {});
        s.check(`${lower(tag)}-truthful-end`, 'the interrupted turn is not presented as a normal completion', {status: last?.status ?? null, text: lastText.slice(-200)},
          last?.status !== 'complete' || /interrupt|stopp|fail|error|lost|exit|crash|unavailable|terminated|killed/i.test(lastText));
      };
      const resume = async tag => {
        const t = await s.turn('root', P.after(tag), {mock: `text:${s.m(`S4X-${tag}`)}`, answer: allowAll});
        s.settled(t.wait);
        s.modelCheck(`${lower(tag)}-reply`, `reply S4X-${tag}-${s.nonce}`, t.reply.slice(0, 120), t.reply.includes(s.m(`S4X-${tag}`)));
        const servers = opencodeServersFor(s.d.identity.pid, s.fixture.dir);
        s.info(`${lower(tag)}-server`, 'one (new) opencode server serves the chat after the kill', servers.map(x => x.pid), servers.length === 1);
      };
      await s.step('sigstop-approval', () => sigstop('STOPA', 'approval'));
      await s.step('kill-approval', () => kill('KILLA', 'approval'));
      await s.step('continue-after-kill', () => resume('AFTERA'));
      await s.step('sigstop-form', () => sigstop('STOPF', 'form'));
      await s.step('kill-form', () => kill('KILLF', 'form'));
      await s.step('continue-after-form-kill', () => resume('AFTERF'));
    },
  },

  S4S: {
    title: 'Provider stalls mid-turn (mock provider with a long --slow-ms, T2)', validation: ['OCE-04'], parity: false, extra: true, permissionMode: 'full', phases: ['run'],
    steps: ['stall-mid-turn', 'continue-after-stall'],
    prompts: n => ({stall: `Write one short sentence that contains the word STALL-${n}. Do not use tools.`, after: `Reply with exactly AFTER-STALL-${n}. Do not use tools.`}),
    unverified: ['LUNA_OPENCODE_ACTIVITY_STALL_MS keeps its default (10 min) in the signed QA app.'],
    async run(s, P) {
      assert.equal(s.tier, 'T2', 'S4S needs the mock provider (T2) started with a long --slow-ms');
      const chatId = s.chatId('root');
      await s.step('stall-mid-turn', async () => {
        // One streamed word, then --slow-ms of silence, then the finish.
        const sent = await s.d.send(chatId, s.p(P.stall, `slow:${s.m('STALL')}`));
        const started = Date.now();
        const limit = Number(s.opts['stall-wait-ms'] ?? 720000);
        let finished = false;
        const waiting = s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, timeoutMs: limit}).then(w => { finished = true; return w; });
        const rows = await timeline(s, 'root', limit + 5000, 15000, () => finished);
        const wait = await waiting;
        const elapsed = Date.now() - started;
        s.evidence.save('stall-timeline.json', {sentAt: new Date(started).toISOString(), elapsedMs: elapsed, rows});
        const firstText = rows.find(r => r.lastParts.includes('text'));
        s.info('stall-first-text', 'partial text reached the transcript before the stall', firstText?.at ?? null, Boolean(firstText));
        s.check('stall-ends-truthfully', 'the stalled turn ends (errored) instead of spinning Working forever', `${wait.result}:${wait.status} after ${Math.round(elapsed / 1000)} s`, wait.result === 'settled');
        // Engine stall notice (provider-neutral): shown while the turn is
        // still working and quiet, lifted by the next provider event.
        const stalledRows = rows.filter(r => r.providerNotice?.kind === 'stalled');
        const firstStalled = stalledRows[0] ?? null;
        s.check('stall-notice-shown', 'a Stalled notice appeared while the quiet turn was still working', firstStalled ? {at: firstStalled.at, status: firstStalled.status, quietSince: firstStalled.providerNotice.atMs} : null,
          Boolean(firstStalled && ['working'].includes(firstStalled.status)));
        if (firstStalled) {
          const quietMs = Date.parse(firstStalled.at) - firstStalled.providerNotice.atMs;
          s.check('stall-notice-window', 'the notice names the last activity at least the 120 s quiet window before it appeared', quietMs, quietMs >= 115000);
        }
        const lastRow = rows.at(-1);
        s.check('stall-notice-cleared', 'the notice is gone once the provider resumed and the turn ended', userView(s, 'root').providerNotice, userView(s, 'root').providerNotice?.kind !== 'stalled');
        s.info('stall-last-row', 'last sampled view', lastRow, true);
        s.info('stall-user-view', 'what the user sees when the stall ends', userView(s, 'root'), true);
        if (wait.result !== 'settled') {
          const stopped = await s.d.stop(chatId);
          s.info('stall-stop', 'Stop after the wait budget', stopped.receipt?.status, stopped.receipt?.status === 'applied');
          await s.d.waitIdle(chatId, {timeoutMs: 60000}).catch(() => null);
        }
        s.d.capture('s4s-after-stall');
      });
      await s.step('continue-after-stall', async () => {
        const t = await s.turn('root', P.after, {mock: `text:${s.m('AFTER-STALL')}`});
        s.settled(t.wait);
        s.check('after-stall-reply', `the chat continues (AFTER-STALL-${s.nonce})`, t.reply.slice(0, 120), t.reply.includes(s.m('AFTER-STALL')));
      });
    },
  },

  S9U: {
    title: 'Unattended answers: the recommended option is picked when nobody answers; Keep waiting; taste questions wait', validation: ['PD-14'],
    parity: true, extra: true, permissionMode: 'full', phases: ['run'],
    steps: ['recommended-auto-pick', 'hold-keeps-waiting', 'taste-question-waits'],
    prompts: n => ({
      auto: `${BLOCKING_QUESTION} Use your tool for asking the user a multiple-choice question (do not ask in plain text). Ask me which caching approach this project should use, with exactly two options labelled "In-memory cache (Recommended)" and "Redis cache". Keep both labels exactly as written, including "(Recommended)". After I answer, reply with exactly CACHE-${n}- followed by the first word of the option I chose, in lowercase. Do not use any other tool.`,
      hold: `${BLOCKING_QUESTION} Use your tool for asking the user a multiple-choice question (do not ask in plain text). Ask me which log format this project should use, with exactly two options labelled "Structured JSON logs (Recommended)" and "Plain text logs". Keep both labels exactly as written, including "(Recommended)". After I answer, reply with exactly LOGS-${n}- followed by the first word of the option I chose, in lowercase. Do not use any other tool.`,
      taste: `${BLOCKING_QUESTION} Use your tool for asking the user a multiple-choice question (do not ask in plain text). Ask me which accent colour I like, with exactly these options: red, green, blue. This is purely my taste, so do not mark any option as recommended. After I answer, reply with exactly ACCENT-${n}- followed by the colour I chose. Do not use any other tool.`,
    }),
    unverified: ['Needs the QA data dir ui-settings.json unattendedAnswerSeconds set to the --unattended-ms value (default 15 s).',
      'Question steps need a real model (T3).'],
    async run(s, P) {
      const chatId = s.chatId('root');
      const delayMs = Number(s.opts['unattended-ms'] ?? 15000);
      const plan = q => q.input?.autoSelect ?? null;
      const card = requestId => s.entries('root').flatMap(e => e.parts ?? []).find(p => p.kind === 'input' && p.requestId === requestId) ?? null;
      const ask = async (prompt, label) => {
        await s.d.waitIdle(chatId).catch(() => null);
        const sent = await s.d.send(chatId, s.p(prompt), {label});
        const first = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, timeoutMs: 180000,
          answer: pending => (pending.questions.some(q => q.approval) ? {approval: 'allow-once'} : null)});
        return {sent, pending: first.result === 'pending' ? first.pending : null, first, shownAt: Date.now()};
      };
      await s.step('recommended-auto-pick', async () => {
        const {sent, pending, first, shownAt} = await ask(P.auto, 'auto-pick question');
        s.check('auto-question-shown', 'the agent asked through the shared question panel', first.result, Boolean(pending));
        if (!pending) return;
        await s.d.holdForCapture('s9u-countdown');
        s.d.capture('s9u-countdown');
        const plans = pending.questions.map(plan);
        s.check('auto-plan-on-card', 'the card carries LUNA\'s unattended-answer plan naming the recommended option', plans,
          plans.length > 0 && plans.every(p => p && !p.fired && p.labels.length === 1 && /recommended/i.test(p.labels[0])));
        const leftMs = plans.map(p => (p?.atMs ?? 0) - shownAt);
        s.check('auto-plan-deadline', `the deadline is at most ${delayMs} ms after the card appeared`, leftMs, leftMs.every(ms => ms <= delayMs + 1000 && ms > -10000));
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, ignore: [pending.requestId], timeoutMs: delayMs + 180000,
          answer: p => (p.questions.some(q => q.approval) ? {approval: 'allow-once'} : null)});
        s.settled(wait, 'auto-picked-turn-settles', ['idle']);
        const resolved = card(pending.requestId);
        s.check('auto-picked-card', 'LUNA answered the card itself: resolved with its plan marked fired', resolved ? {resolved: resolved.resolved, fired: (resolved.questions ?? []).map(q => plan(q)?.fired ?? null)} : null,
          Boolean(resolved?.resolved && (resolved.questions ?? []).every(q => plan(q)?.fired === true)));
        s.check('auto-no-driver-answer', 'the driver sent no answer for this request', wait.handled.filter(h => h.requestId === pending.requestId), !wait.handled.some(h => h.requestId === pending.requestId));
        const reply = s.replyAfter('root', sent.messageId);
        s.modelCheck('auto-answer-consumed', `the agent continued with the recommended option (CACHE-${s.nonce}-in-memory)`, reply.slice(0, 200), reply.toLowerCase().includes(`cache-${s.nonce}-in`.toLowerCase()));
        s.d.capture('s9u-auto-picked');
      });
      await s.step('hold-keeps-waiting', async () => {
        const {sent, pending} = await ask(P.hold, 'held question');
        s.check('hold-question-shown', 'the agent asked through the shared question panel', Boolean(pending), Boolean(pending));
        if (!pending) return;
        s.check('hold-plan-on-card', 'the card has an unattended-answer plan before the hold', pending.questions.map(plan), pending.questions.every(q => plan(q)));
        const held = await s.d.holdInput(chatId, pending.requestId);
        s.check('hold-applied', 'Keep waiting (holdInput) was applied by the host', {status: held.receipt?.status, reason: held.receipt?.reason ?? null}, held.receipt?.status === 'applied');
        const quiet = await s.stays('root', () => Boolean(card(pending.requestId)?.resolved), delayMs + 10000);
        s.check('hold-still-open', `the held question is still open ${Math.round((delayMs + 10000) / 1000)} s later`, quiet, quiet);
        const {receipt} = await s.d.answer(chatId, pending, {pick: 'Plain text logs'});
        s.check('hold-user-answer', 'the user\'s own answer is applied after holding', receipt?.status, receipt?.status === 'applied');
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, ignore: [pending.requestId], timeoutMs: 180000});
        s.settled(wait, 'held-turn-settles', ['idle']);
        const resolved = card(pending.requestId);
        s.check('hold-not-fired', 'the card is resolved by the user, not marked as LUNA\'s pick', (resolved?.questions ?? []).map(q => plan(q)?.fired ?? null), Boolean(resolved?.resolved) && !(resolved?.questions ?? []).some(q => plan(q)?.fired));
        const reply = s.replyAfter('root', sent.messageId);
        s.modelCheck('hold-answer-consumed', `the agent used the user's pick (LOGS-${s.nonce}-plain)`, reply.slice(0, 200), reply.toLowerCase().includes(`logs-${s.nonce}-plain`.toLowerCase()));
      });
      await s.step('taste-question-waits', async () => {
        const {sent, pending} = await ask(P.taste, 'taste question');
        s.check('taste-question-shown', 'the agent asked through the shared question panel', Boolean(pending), Boolean(pending));
        if (!pending) return;
        s.modelCheck('taste-no-recommendation', 'the agent marked no option as recommended', pending.questions.map(q => q.options), !pending.questions.some(q => (q.options ?? []).some(o => /recommended/i.test(o))));
        s.check('taste-no-plan', 'a question without a recommendation gets no unattended-answer plan', pending.questions.map(plan),
          pending.questions.some(q => (q.options ?? []).some(o => /recommended/i.test(o))) || pending.questions.every(q => !plan(q)));
        const quiet = await s.stays('root', () => Boolean(card(pending.requestId)?.resolved), delayMs + 10000);
        s.check('taste-waits', `the question is still open ${Math.round((delayMs + 10000) / 1000)} s later`, quiet, quiet);
        const {receipt} = await s.d.answer(chatId, pending, {pick: 'green'});
        s.check('taste-user-answer', 'the user\'s answer is applied', receipt?.status, receipt?.status === 'applied');
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, ignore: [pending.requestId], timeoutMs: 180000});
        s.settled(wait, 'taste-turn-settles', ['idle']);
        const reply = s.replyAfter('root', sent.messageId);
        s.modelCheck('taste-answer-consumed', `reply is ACCENT-${s.nonce}-green`, reply.slice(0, 200), reply.toLowerCase().includes(`accent-${s.nonce}-green`.toLowerCase()));
      });
    },
  },

  S9Q: {
    title: 'Nonblocking questions (LUNA ask_user): agent keeps working, late answer as a new turn, amendment, steer while working, idempotent repeat', validation: ['PD-14'],
    parity: true, extra: true, permissionMode: 'full', phases: ['run'],
    steps: ['ask-and-keep-working', 'late-answer-new-turn', 'amend-answer', 'idempotent-repeat', 'answer-steers-running-turn'],
    prompts: n => ({
      ask: `Use LUNA's ask_user tool (not your built-in question tool) to ask me which date format the report should use, with exactly two options labelled "ISO 8601 (Recommended)" and "US format". Do not wait for my answer: right after asking, reply with exactly ASKED-${n} and end your turn. Later, whenever my answer to that question arrives, reply with exactly FORMAT-${n}- followed by the first word of the option I chose, in lowercase, and nothing else.`,
      steer: `Use LUNA's ask_user tool (not your built-in question tool) to ask me which log level to use, with exactly two options labelled "Info (Recommended)" and "Debug". Do not wait for my answer. Then use your shell tool to run exactly this command: python3 -c "import time; time.sleep(25); print('WORK-${n}')". After the command finishes, reply with WORK-${n} and, if my answer to the log-level question has arrived by then, also LEVEL-${n}- followed by my choice in lowercase.`,
    }),
    unverified: ['Needs a provider session with LUNA tools attached (T3; Claude MCP, Codex dynamic/MCP or OpenCode MCP delivery).'],
    async run(s, P) {
      const chatId = s.chatId('root');
      const cards = () => s.entries('root').flatMap(e => (e.parts ?? []).filter(p => p.kind === 'asyncQuestion'));
      const userTexts = () => s.entries('root').filter(e => e.role === 'user').map(e => textOf(e));
      const askTools = messageId => s.toolsAfter('root', messageId).filter(t => /ask_user$/.test(t.call?.tool ?? t.call?.name ?? ''));
      const latestReply = () => textOf(s.entries('root').filter(e => e.role === 'assistant').at(-1) ?? {});
      const waitReply = async (marker, label, timeoutMs = 240000) => {
        const mirror = s.d.chatByRole('root').mirror;
        // The reply must be complete, not still streaming: the session can
        // read idle a moment before the final text lands.
        await waitOn([mirror, s.d.sessions], () => !s.d.busy(chatId) && mirror.entries.filter(e => e.role === 'assistant')
          .some(e => e.status === 'complete' && textOf(e).toLowerCase().includes(marker.toLowerCase())) ? true : null,
        timeoutMs, label).catch(() => null);
        await sleep(2000);
        await s.d.waitIdle(chatId, {timeoutMs: 120000}).catch(() => null);
      };
      let card = null;
      await s.step('ask-and-keep-working', async () => {
        const t = await s.turn('root', P.ask, {answer: pending => (pending.questions.some(q => q.approval) ? {approval: 'allow-once'} : null)});
        s.check('turn-did-not-wait', 'the turn ended without anyone answering (no blocking question)', t.wait.result, t.wait.result === 'settled');
        s.check('never-awaiting-input', 'the chat never entered awaitingInput', t.wait.statuses, !(t.wait.statuses ?? []).includes('awaitingInput'));
        s.modelCheck('asked-reply', `reply is ASKED-${s.nonce}`, t.reply.slice(0, 160), t.reply.includes(s.m('ASKED')));
        const tools = askTools(t.messageId);
        s.check('ask-user-tool', 'the agent called LUNA ask_user and it returned without error', tools.map(x => `${x.call?.name}:${x.resolved}:${x.isError}`), tools.some(x => x.resolved && !x.isError));
        card = cards().at(-1) ?? null;
        await s.d.holdForCapture('s9q-async-card');
        s.d.capture('s9q-async-card');
        s.check('card-open', 'an open nonblocking card holds the question and options', card ? {id: card.id, resolved: card.resolved, questions: card.questions} : null,
          Boolean(card && !card.resolved && (card.questions?.[0]?.options ?? []).some(o => /ISO 8601/.test(o)) && (card.questions?.[0]?.options ?? []).some(o => /US format/.test(o))));
      });
      await s.step('late-answer-new-turn', async () => {
        if (!card) throw new Error('no async card to answer');
        const mark = s.d.sessions.mark(chatId);
        const {receipt} = await s.d.answerAsync(chatId, card.id, [{questionId: '0', labels: ['US format']}]);
        s.check('late-answer-applied', 'the late answer is applied after the turn ended', receipt?.status, receipt?.status === 'applied');
        await waitReply(s.m('FORMAT'), 'reply to the late answer');
        const delivered = userTexts().find(text => text.includes(`[luna-question:${card.id}]`) && text.includes('US format'));
        s.check('answer-message-tagged', 'the answer reached the agent as a user message tagged with the card id', delivered?.slice(0, 200) ?? null, Boolean(delivered));
        const now = cards().find(c => c.id === card.id);
        s.check('card-answered', 'the card records one answer and reads as answered', now ? {resolved: now.resolved, answers: (now.answers ?? []).length} : null, Boolean(now?.resolved && (now.answers ?? []).length === 1));
        s.check('new-turn-ran', 'a new turn ran for the answer', s.d.sessions.since(chatId, mark).map(h => h.status), s.d.sessions.since(chatId, mark).some(h => h.status === 'working'));
        s.modelCheck('late-answer-consumed', `reply is FORMAT-${s.nonce}-us`, latestReply().slice(0, 160), latestReply().toLowerCase().includes(`format-${s.nonce}-us`.toLowerCase()));
        s.d.capture('s9q-answered-card');
      });
      await s.step('amend-answer', async () => {
        const {receipt} = await s.d.answerAsync(chatId, card.id, [{questionId: '0', labels: ['ISO 8601 (Recommended)']}], {label: 'amend async answer'});
        s.check('amend-applied', 'a different later answer is applied as an amendment', receipt?.status, receipt?.status === 'applied');
        await waitReply(`${s.m('FORMAT')}-iso`, 'reply to the amendment');
        const amended = userTexts().find(text => text.startsWith('I changed my answer') && text.includes(`[luna-question:${card.id}]`) && text.includes('ISO 8601'));
        s.check('amend-message', 'the amendment is a new user message saying it replaces the earlier answer', amended?.slice(0, 200) ?? null, Boolean(amended));
        const now = cards().find(c => c.id === card.id);
        s.check('card-history', 'the card keeps both answers in order', (now?.answers ?? []).map(a => a.answers?.[0]?.labels ?? []), (now?.answers ?? []).length === 2);
        s.modelCheck('amend-consumed', `reply is FORMAT-${s.nonce}-iso`, latestReply().slice(0, 160), latestReply().toLowerCase().includes(`format-${s.nonce}-iso`.toLowerCase()));
      });
      await s.step('idempotent-repeat', async () => {
        const before = userTexts().length;
        const {receipt} = await s.d.answerAsync(chatId, card.id, [{questionId: '0', labels: ['ISO 8601 (Recommended)']}], {label: 'repeat async answer'});
        s.check('repeat-applied', 'repeating the latest answer is accepted', {status: receipt?.status, reason: receipt?.reason?.message ?? null}, receipt?.status === 'applied');
        const quiet = await s.stays('root', entries => entries.filter(e => e.role === 'user').length > before, 8000);
        s.check('repeat-not-delivered', 'no second message is delivered for the same answer', quiet, quiet);
        s.check('repeat-history', 'the card history is unchanged', (cards().find(c => c.id === card.id)?.answers ?? []).length, (cards().find(c => c.id === card.id)?.answers ?? []).length === 2);
      });
      await s.step('answer-steers-running-turn', async () => {
        await s.d.waitIdle(chatId).catch(() => null);
        const known = new Set(cards().map(c => c.id));
        const sent = await s.d.send(chatId, s.p(P.steer), {label: 'steer question'});
        const mirror = s.d.chatByRole('root').mirror;
        const fresh = await mirror.waitFor(() => cards().find(c => !known.has(c.id)) ?? null, {timeoutMs: 180000, label: 'second async card'}).catch(() => null);
        s.check('second-card', 'the agent asked through ask_user and kept working', fresh ? {id: fresh.id, busy: s.d.busy(chatId)} : null, Boolean(fresh));
        if (!fresh) return;
        const busyWhenAnswered = s.d.busy(chatId);
        const {receipt} = await s.d.answerAsync(chatId, fresh.id, [{questionId: '0', labels: ['Debug']}], {label: 'answer while working'});
        s.check('steer-answer-applied', 'the answer sent while the agent is still working is applied', {status: receipt?.status, reason: receipt?.reason?.message ?? null, busyWhenAnswered}, receipt?.status === 'applied');
        s.info('answered-while-busy', 'the agent was still working when the answer was sent', busyWhenAnswered, busyWhenAnswered);
        const wait = await s.d.waitTurn(chatId, {mark: sent.mark, messageId: sent.messageId, timeoutMs: 300000});
        s.settled(wait, 'steer-turn-settles', ['idle']);
        // The steered answer opens a boundary inside the turn; read the
        // transcript only once the final WORK reply is in.
        await waitReply(s.m('WORK'), 'final reply after the steered answer', 300000);
        const delivered = userTexts().some(text => text.includes(`[luna-question:${fresh.id}]`) && text.includes('Debug'));
        s.check('steer-answer-delivered', 'the answer reached the agent tagged with the card id', delivered, delivered);
        const after = s.after('root', sent.messageId);
        const answerAt = after.findIndex(e => e.role === 'user' && textOf(e).includes(`[luna-question:${fresh.id}]`));
        const finalAt = after.findLastIndex(e => e.role === 'assistant' && textOf(e).includes(s.m('WORK')));
        s.check('steer-inside-turn', 'the answer landed inside the running turn, before its final reply', {answerAt, finalAt}, answerAt >= 0 && finalAt > answerAt);
        // Whether the model folds a mid-turn answer into its reply is model
        // behaviour; LUNA's part is the delivery checked above.
        const replies = after.filter(e => e.role === 'assistant').map(e => textOf(e)).join(' / ');
        s.info('steer-answer-consumed', `the agent reported LEVEL-${s.nonce}-debug`, replies.slice(-200), replies.toLowerCase().includes(`level-${s.nonce}-debug`.toLowerCase()));
        s.d.capture('s9q-steered');
      });
    },
  },

  S9S: {
    title: 'Live steer: an image steer row lands in the running turn; a steer racing turn completion is delivered once', validation: ['PD-14', 'SW-06 E07'],
    parity: true, extra: true, permissionMode: 'full', phases: ['run'],
    steps: ['steer-applied', 'completion-race'],
    prompts: n => ({
      busy: `Use your shell tool to run exactly this command: sleep 15\nThen reply with exactly WAITED-${n}, and also follow any instruction I add while you are waiting.`,
      steer: `While you wait: the attached image has one dominant colour. In your final reply also write STEERED-${n}- followed by that colour as one lowercase word. Do not use any other tool.`,
      short: `Reply with exactly RACE-${n}-A. Do not use tools.`,
      race: `Reply with exactly RACE-${n}-B. Do not use tools.`,
    }),
    unverified: ['The provider-record side (Claude session JSONL, Codex rollout) is compared by the SW-06 analysis, not here.',
      'Which side of the completion race wins is timing-dependent; both truthful outcomes are accepted and recorded.'],
    async run(s, P) {
      const chatId = s.chatId('root');
      const chat = s.d.chatByRole('root');
      const queued = id => (chat.queue.latest?.items ?? []).find(item => item.id === id) ?? null;
      const rows = id => s.entries('root').filter(e => e.id === id);
      const workingRuns = mark => s.d.sessions.since(chatId, mark).map(h => h.status)
        .filter((status, i, all) => status === 'working' && all[i - 1] !== 'working').length;
      await s.step('steer-applied', async () => {
        const mark = s.d.sessions.mark(chatId);
        const busy = await s.d.send(chatId, s.p(P.busy, 'tool-shell:sleep 15'));
        await waitOn([s.d.sessions, chat.mirror], () => s.d.busy(chatId) || null, 60000, 'busy').catch(() => null);
        // Let the provider start its shell call so the steer arrives mid-turn.
        await sleep(4000);
        const staged = await s.d.upload(solidPng(64, 64, [130, 30, 200]), 'qa-purple.png');
        const steer = await s.d.send(chatId, s.p(P.steer, 'image'), {attachments: [staged.path], queue: true, followUpBehavior: 'steer'});
        s.check('steer-queued', 'the busy chat accepts the steer row through queueMessage', {path: steer.path, receipt: steer.receipt?.status},
          steer.path === 'queueMessage' && ['applied', 'pending'].includes(steer.receipt?.status));
        const left = await waitOn([chat.queue, s.d.sessions], () => (!queued(steer.messageId) ? {busy: s.d.busy(chatId)} : null), 30000, 'steer leaves queue').catch(() => null);
        s.check('steer-claimed-live', 'the steer row leaves the queue while the turn is still running', left, Boolean(left?.busy));
        const wait = await s.d.waitTurn(chatId, {mark: busy.mark, messageId: busy.messageId, timeoutMs: s.turnTimeout});
        s.settled(wait);
        const quiet = await s.stays('root', entries => entries.some(e => e.status === 'streaming'), 6000);
        s.check('single-turn', 'the steer joined the running turn: one working period, no extra turn', {runs: workingRuns(mark), quiet}, workingRuns(mark) === 1 && quiet);
        s.check('steer-row-once', 'the steered row appears exactly once', rows(steer.messageId).length, rows(steer.messageId).length === 1);
        const entries = s.entries('root');
        const at = entries.findIndex(e => e.id === steer.messageId);
        const after = entries.slice(at + 1).filter(e => e.role === 'assistant').map(textOf).join('\n');
        s.check('steer-before-turn-end', 'the steered row sits before the turn\'s final reply', at, at > entries.findIndex(e => e.id === busy.messageId) && /\S/.test(after));
        s.modelCheck('steer-consumed', `the reply carries STEERED-${s.nonce}-purple`, after.slice(-200), after.toLowerCase().includes(`steered-${s.nonce}-purple`.toLowerCase()));
        s.ids.steer = {busy: busy.messageId, steer: steer.messageId, image: staged};
      });
      await s.step('completion-race', async () => {
        const mark = s.d.sessions.mark(chatId);
        const first = await s.d.send(chatId, s.p(P.short, `text:${s.m('RACE')}-A`));
        await waitOn([s.d.sessions, chat.mirror], () => s.d.busy(chatId) || null, 60000, 'race busy').catch(() => null);
        // Queued the moment the short turn is visibly running, so the steer
        // races the reply's completion.
        const race = await s.d.send(chatId, s.p(P.race, `text:${s.m('RACE')}-B`), {queue: true, followUpBehavior: 'steer'});
        s.check('race-queued', 'the steer row is accepted while the turn runs', race.receipt?.status, ['applied', 'pending'].includes(race.receipt?.status));
        await s.d.waitTurn(chatId, {mark: first.mark, messageId: first.messageId, timeoutMs: s.turnTimeout});
        await s.d.waitIdle(chatId, {timeoutMs: 120000}).catch(() => null);
        const landed = await waitOn([chat.mirror, chat.queue], () => (rows(race.messageId).length || queued(race.messageId)?.holdReason ? true : null), 30000, 'race outcome').catch(() => null);
        const held = queued(race.messageId);
        let outcome = rows(race.messageId).length ? 'delivered' : held ? `held:${held.holdReason ?? 'none'}` : 'missing';
        s.info('race-outcome', 'which side of the race won (steered in-turn, next turn, or held for an explicit send)', {outcome, runs: workingRuns(mark)}, Boolean(landed));
        s.check('race-not-lost', 'the steer row is either delivered or visibly held with a reason, never lost', outcome, outcome !== 'missing' && outcome !== 'held:none');
        if (held) {
          const {receipt} = await s.d.sendQueued(chatId, race.messageId);
          s.check('held-send-receipt', 'an explicit Send now of the held row is accepted', receipt?.status, ['applied', 'pending'].includes(receipt?.status));
          await waitOn([chat.mirror, s.d.sessions], () => (s.d.replySettled(s.entries('root'), race.messageId) && !s.d.busy(chatId) ? true : null), s.turnTimeout, 'held row delivered').catch(() => null);
          outcome = `${outcome}->sent`;
        }
        const quiet = await s.stays('root', entries => entries.filter(e => e.id === race.messageId).length > 1, 6000);
        s.check('race-row-once', 'the race row is delivered exactly once', {outcome, count: rows(race.messageId).length, quiet}, rows(race.messageId).length === 1 && quiet);
        s.check('queue-empty', 'no row is left behind in the queue', (chat.queue.latest?.items ?? []).map(i => i.id), !queued(race.messageId));
        const reply = s.replyAfter('root', race.messageId) || s.replyAfter('root', first.messageId);
        s.modelCheck('race-consumed', `a reply carries RACE-${s.nonce}-B`, reply.slice(-160), reply.includes(`RACE-${s.nonce}-B`));
        s.ids.race = {first: first.messageId, race: race.messageId, outcome};
      });
    },
  },
  S8X: {
    title: 'Automatic compaction mid-turn and manual compaction ordering (small-context mock, T2)', validation: ['OCE-14'], parity: false, extra: true, permissionMode: 'full', phases: ['run'],
    steps: ['warm-up', 'large-tool-turn', 'next-turn', 'manual-compaction', 'after-compaction'],
    prompts: n => ({warm: `Reply with exactly WARM-${n}. Do not use tools.`, big: `Run the shell command you are given and then reply with BIG-${n}.`, after: `Reply with exactly AFTER-COMPACT-${n}. Do not use tools.`}),
    unverified: ['Compaction thresholds depend on the mock model limit configured in the isolated profile (recorded in result.md).'],
    async run(s, P) {
      assert.equal(s.tier, 'T2', 'S8X needs the small-context mock model (T2)');
      const chatId = s.chatId('root');
      const mirror = s.d.chatByRole('root').mirror;
      const controls = [];
      const record = () => {
        const json = JSON.stringify(mirror.latest.contextControl ?? null);
        if (controls.at(-1)?.json !== json) controls.push({at: now(), json, session: s.d.sessions.status(chatId)});
      };
      mirror.on('change', record);
      s.d.sessions.on('change', record);
      const compactRows = since => controls.filter(c => c.at >= since).map(c => ({at: c.at, session: c.session, compact: JSON.parse(c.json)?.compact ?? null}));
      try {
        await s.step('warm-up', async () => {
          const t = await s.turn('root', P.warm, {mock: `text:${s.m('WARM')}`});
          s.settled(t.wait);
        });
        // The mock answers a tool result with text, so one turn cannot chain
        // two tool rounds: the large turn overflows the small window and the
        // automatic compaction lands at its end or at the start of the next
        // turn. Both turns are observed and must each complete exactly once.
        const autoSince = now();
        await s.step('large-tool-turn', async () => {
          const t = await s.turn('root', P.big, {mock: `tool-shell:${s.opts['big-command'] ?? 'seq 1 6000'}`});
          s.settled(t.wait);
          s.ids.big = t.messageId;
          s.info('large-turn-statuses', 'session statuses during the large turn', s.d.sessions.statusesSince(chatId, t.mark), true);
        });
        await s.step('next-turn', async () => {
          const t = await s.turn('root', P.after.replace('AFTER-COMPACT', 'NEXT'), {mock: `text:NEXT-${s.nonce}`});
          s.settled(t.wait);
          s.info('next-turn-statuses', 'session statuses during the next turn', s.d.sessions.statusesSince(chatId, t.mark), true);
          const rows = compactRows(autoSince);
          s.evidence.save('auto-compaction-controls.json', rows);
          const auto = rows.filter(r => r.compact && r.compact.status !== 'idle');
          s.check('auto-compaction-seen', 'automatic compaction ran (context-control lifecycle) after the window overflowed', auto.map(r => `${r.at.slice(11, 23)} ${r.compact.status}:${r.compact.mechanism ?? ''} session=${r.session}`), auto.length > 0);
          for (const [label, id] of [['large', s.ids.big], ['next', t.messageId]]) {
            const rest = s.after('root', id);
            const nextUser = rest.findIndex(e => e.role === 'user');
            const replies = (nextUser < 0 ? rest : rest.slice(0, nextUser)).filter(e => e.role === 'assistant');
            s.info(`${label}-turn-entries`, `assistant entries of the ${label} turn (status and part kinds)`, replies.map(e => ({status: e.status, parts: (e.parts ?? []).map(p => p.kind)})), replies.length >= 1);
          }
          s.modelCheck('next-turn-reply', `the turn after compaction completes (NEXT-${s.nonce})`, t.reply.slice(0, 120), t.reply.includes(`NEXT-${s.nonce}`));
        });
        await s.step('manual-compaction', async () => {
          await s.d.waitIdle(chatId).catch(() => null);
          const since = now();
          const compact = await s.d.compact(chatId);
          const atReceipt = JSON.parse(JSON.stringify(mirror.latest.contextControl ?? null));
          s.check('compact-receipt', 'compactContext applied', {status: compact.receipt?.status, reason: compact.receipt?.reason?.code ?? null}, compact.receipt?.status === 'applied');
          await mirror.waitFor(() => {
            const c = mirror.latest.contextControl?.compact;
            return c && c.status && c.status !== 'running' && controls.some(r => r.at >= since && JSON.parse(r.json)?.compact?.status === 'running');
          }, {timeoutMs: 180000, label: 'manual compaction ended'}).catch(() => null);
          const rows = compactRows(since);
          s.evidence.save('manual-compaction-controls.json', {atReceipt, rows});
          const statuses = rows.map(r => r.compact?.status ?? null).filter(Boolean);
          s.check('manual-lifecycle', 'manual compaction shows running then a terminal status', statuses, statuses.includes('running') && statuses.some(x => x !== 'running'));
          const firstTerminal = rows.findIndex(r => r.compact && r.compact.status !== 'running');
          s.info('session-during-compaction', 'session status while manual compaction runs (LUNA keeps the chat idle; compaction is context-control lifecycle)', rows.slice(0, Math.max(0, firstTerminal)).filter(r => r.compact?.status === 'running').map(r => r.session), rows.filter(r => r.compact?.status === 'running').every(r => r.session !== 'idle'));
        });
        await s.step('after-compaction', async () => {
          const t = await s.turn('root', P.after, {mock: `text:${s.m('AFTER-COMPACT')}`});
          s.settled(t.wait);
          s.check('after-compaction-reply', `the chat continues (AFTER-COMPACT-${s.nonce})`, t.reply.slice(0, 120), t.reply.includes(s.m('AFTER-COMPACT')));
        });
      } finally {
        mirror.off('change', record);
        s.d.sessions.off('change', record);
        s.evidence.save('context-controls.json', controls);
      }
    },
  },
};

// ------------------------------------------------------------------ run
function chooseModel(catalog, {provider, tier, model}) {
  if (model) return catalog.find(m => m.id === model);
  if (provider === 'opencode' && tier === 'T2') return catalog.find(m => m.id.startsWith('luna-mock/'));
  if (provider === 'opencode') return catalog.find(m => /^opencode\/.*(-free|big-pickle)$/.test(m.id)) ?? catalog.find(m => m.isDefault);
  return catalog.find(m => m.isDefault) ?? catalog[0];
}

async function runScenario(id, opts, {rehearsal = null} = {}) {
  const scenario = SCENARIOS[id];
  assert.ok(scenario, `unknown scenario ${id}; try: list`);
  const provider = opts.provider;
  const codexQuestions = provider === 'codex' && ['S9Q', 'S9S'].includes(id) && !rehearsal;
  assert.ok(PROVIDERS.includes(provider) || codexQuestions,
    `--provider must be one of ${PROVIDERS.join(', ')} (Codex is supported only for live S9Q and S9S)`);
  if (codexQuestions) {
    assert.equal(opts.tier, 'T3', `Codex ${id} requires --tier T3`);
    assert.ok(opts.model?.trim(), `Codex ${id} requires an explicit --model from ListModels`);
  }
  const phase = opts.phase ?? scenario.phases[0];
  assert.ok(scenario.phases.includes(phase), `${id} phases: ${scenario.phases.join(', ')}`);
  const tier = rehearsal ? 'R0' : opts.tier;
  if (!rehearsal) {
    assert.ok(['T2', 'T3'].includes(tier), '--tier T2 (mock provider) or T3 (real model) is required (QF-05)');
    assert.ok(!(provider === 'claude-code' && tier === 'T2'), 'Claude comparison runs are real-model runs: use --tier T3 (PD-18)');
  }
  const identity = rehearsal ? assertRehearsalIdentity(rehearsal.child, rehearsal.port) : assertQaIdentity({provider});
  let evidence;
  let resumed = null;
  if (phase === 'post') {
    assert.ok(opts.run, '--phase post needs --run <pre-phase run dir>');
    const dir = path.resolve(opts.run);
    resumed = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
    assert.equal(resumed.scenario, id, 'the run directory belongs to another scenario');
    assert.equal(resumed.requested.provider, provider, 'the run directory belongs to another provider');
    assert.ok(fs.existsSync(path.join(dir, 'restart-required.json')), 'the pre phase did not finish (no restart-required.json)');
    evidence = new Evidence(dir, resumed);
    evidence.criteria = (resumed.criteria ?? []).filter(c => c.criterion !== 'secret-scan' && c.criterion !== 'wire-log-captured');
  } else {
    const scenarioRoot = path.join(path.resolve(opts['evidence-root'] ?? path.join(root, 'target/qa/opencode')), id.toLowerCase(), provider);
    fs.mkdirSync(scenarioRoot, {recursive: true});
    const dir = fs.mkdtempSync(path.join(scenarioRoot, `run-${new Date().toISOString().replace(/[-:.]/g, '').slice(0, 15)}Z-`));
    fs.writeFileSync(path.join(scenarioRoot, 'latest'), `${dir}\n`);
    evidence = new Evidence(dir, null);
  }
  console.error(`evidence: ${evidence.dir}`);
  const nonce = resumed?.nonce ?? crypto.randomBytes(4).toString('hex').toUpperCase();
  const prompts = scenario.prompts(nonce);
  const rpc = new Rpc({port: identity.port, log: record => evidence.command({...record, phase})});
  const manifest = resumed ?? {
    schema: 'luna-opencode-qa-scenario/1', mode: 'scenario', scenario: id, title: scenario.title, validation: scenario.validation,
    tier, rehearsal: Boolean(rehearsal), nonce, startedAt: now(), luna: lunaSource(),
    app: {name: identity.appName, bundleId: identity.bundleId, port: identity.port, pid: identity.pid, executable: identity.executable,
      dataDir: identity.dataDir, codesign: rehearsal ? 'n/a (rehearsal)' : 'verified (--deep --strict)', mcpIsolation: identity.lsenv.LUNA_QA_MCP_ISOLATION ?? null},
    requested: {provider, model: opts.model ?? null, variant: opts.variant ?? null, permissionMode: scenario.permissionMode},
    xdg: identity.profile ? {isolated: true, profile: identity.profile.dir, homeIsolated: identity.profile.homeIsolated, offline: identity.profile.offline, projectConfigDisabled: identity.profile.projectConfigDisabled} : null,
    server: provider === 'opencode' && !rehearsal ? {ownership: 'spawned by the LUNA adapter (--stdio)', credentials: 'LUNA-private; never captured'} : null,
    plannedSteps: scenario.steps, sends: [], tasks: {}, phases: {},
    replay: ['node', 'scripts/qa-opencode-scenarios.mjs', ...process.argv.slice(2)].join(' '),
  };
  evidence.manifest = manifest;
  evidence.phase = phase;
  manifest.phases[phase] = {startedAt: now(), pid: identity.pid};
  manifest.wireLog = manifest.wireLog ?? null;
  const wireStart = provider === 'opencode' && !rehearsal ? wireOffsets(identity.wireDir) : null;
  manifest.phases[phase].wireStart = wireStart?.files ?? null;
  if (!resumed && provider === 'opencode' && !rehearsal) {
    const out = evidence.file('binary-resolution.json');
    const argv = [path.join(root, 'scripts/qa-opencode-manifest.py'), 'binary', '--path', identity.profile.wrapper, '--out', out];
    const record = spawnSync('python3', argv, {encoding: 'utf8'});
    evidence.command({method: 'binary-record', params: {argv: ['python3', ...argv]}, outcome: record.status === 0 ? 'ok' : 'err'});
    manifest.opencodeBinary = fs.existsSync(out) ? JSON.parse(fs.readFileSync(out, 'utf8')) : {error: record.stderr};
  }
  evidence.writeManifest();

  await rpc.connect();
  const driver = new Driver({rpc, evidence, identity, provider, nonce, rpcTimeoutMs: Number(opts['rpc-timeout'] ?? 45000), ax: Boolean(opts.ax),
    captureHoldMs: Number(opts['capture-hold-ms'] ?? 0)});
  let failure = null;
  let stopped = [];
  try {
    const engine = await driver.init();
    evidence.save(`engine-info-${phase}.json`, engine);
    let config;
    let fixture;
    if (resumed) {
      config = resumed.config;
      fixture = resumed.fixtureInfo;
      for (const [role, chatId] of Object.entries(resumed.tasks)) driver.attach(chatId, {role, config, cwd: fixture.dir, spaceId: resumed.projectId});
      await Promise.all([...driver.chats.values()].map(c => c.mirror.ready));
    } else {
      const catalog = await rpc.call('ListModels', {harness: provider});
      evidence.save('catalog.json', catalog);
      const model = chooseModel(catalog, {provider, tier: rehearsal ? 'T3' : tier, model: opts.model});
      evidence.check('model-in-catalog', 'requested or tier default model listed by ListModels', model?.id ?? null, Boolean(model));
      assert.ok(model, 'no usable model in the LUNA catalog');
      if (!rehearsal) {
        const mock = model.id.startsWith('luna-mock/');
        evidence.check('tier-label', tier === 'T2' ? 'T2 uses the luna-mock provider' : 'T3 uses a real (non-mock) model', model.id, tier === 'T2' ? mock : !mock);
      }
      const variant = opts.variant ?? null;
      config = {harness: provider, model: model.id, reasoning: variant, modelOptions: {}, permissionMode: scenario.permissionMode,
        effectivePermissionMode: scenario.permissionMode, sandbox: scenario.permissionMode === 'full' ? 'danger-full-access' : 'workspace-write'};
      manifest.requested.model = model.id;
      manifest.config = config;
      const fx = makeFixture();
      scenario.fixture?.(fx, nonce);
      fixture = {dir: fx.dir, sha: fx.sha ?? null};
      manifest.fixture = fx.dir;
      manifest.fixtureInfo = fixture;
      evidence.writeManifest();
      const spaceId = await driver.createProject(fx.dir);
      await driver.createChat({role: 'root', spaceId, cwd: fx.dir, config, title: `OpenCode QA ${id} ${nonce}`});
      fixture.git = fx.git;
    }
    const context = new ScenarioContext({driver, evidence, provider, tier, nonce, fixture, config, opts});
    const fn = phase === 'post' ? scenario.post : phase === 'pre' ? scenario.pre : scenario.run;
    await driver.runStep('setup', () => driver.snapshot(`before-${phase}`));
    await fn.call(scenario, context, prompts);
    await driver.snapshot(`after-${phase}`);
    const record = await driver.chatRecord(driver.chatByRole('root').id).catch(() => null);
    manifest.effective = {config: record?.config ?? null, title: record?.title ?? null, providerSession: record?.harnessSessionId ?? null};
  } catch (error) {
    failure = error;
    evidence.save(`failure-${phase}-${Date.now()}.json`, {message: String(error?.message ?? error), stack: error?.stack, unknown: Boolean(error?.unknown), time: now()});
    evidence.check('scenario-completed', 'every step ran to completion', String(error?.message ?? error).slice(0, 300), false);
    if (!(error instanceof UnknownOutcome)) stopped = await driver.cleanup().catch(e => [{error: String(e)}]);
    else stopped = [{skipped: 'an unknown send stops the run without further writes (QF-17)'}];
  }
  manifest.phases[phase].endedAt = now();
  manifest.phases[phase].cleanup = stopped;
  if (provider === 'opencode' && !rehearsal) {
    const slice = captureWireSlice(evidence, identity.wireDir, wireStart?.files);
    manifest.phases[phase].wire = slice;
    manifest.wireLog = {dir: identity.wireDir, present: slice.present};
    evidence.check(`wire-log-captured`, 'LUNA_OPENCODE_WIRE_LOG present with bytes for this phase (QF-11)', `${identity.wireDir ?? 'unset'}; ${slice.bytes} bytes; ${slice.frames} frames; ${slice.http} http`, slice.present && slice.bytes > 0);
    if (id === 'S7') verifyImageBytes(evidence, driver);
  }
  driver.close();
  const usage = evidence.manifest.sends.filter(s => ['message', 'sideChatMessage', 'review', 'history'].includes(s.kind)).length;
  manifest.modelUsage = {provider, model: manifest.requested.model, tier, providerTurnsRequested: usage, note: 'turns requested through LUNA; consumption per provider accounting is not asserted'};
  const passed = finishEvidence(evidence, {
    title: `OpenCode QA scenario ${id}: ${scenario.title} (${provider}, ${phase})`, tier, rehearsal: Boolean(rehearsal),
    writePass: phase !== 'pre',
    notes: [`Validation rows: ${scenario.validation.join(', ')}.`, `Nonce ${nonce}; prompts are provider-neutral (QF-18).`,
      ...(phase === 'pre' ? ['Pre phase only: restart the QA app gracefully, then run --phase post --run this directory (QF-24).'] : [])],
    unverified: scenario.unverified ?? [],
  });
  console.log(JSON.stringify({scenario: id, provider, tier, phase, passed, evidence: evidence.dir, failure: failure ? String(failure.message ?? failure) : null}));
  return {passed, evidence: evidence.dir, failure};
}

// QF-08 image path: the staged bytes equal the files[] the adapter sent.
function verifyImageBytes(evidence, driver) {
  const staged = new Map((evidence.manifest.attachments ?? []).map(a => [a.sha256, a]));
  const seen = [];
  for (const record of readJsonl(evidence.file('http.jsonl'))) {
    const files = record.request?.body?.files;
    if (!Array.isArray(files)) continue;
    for (const file of files) {
      const uri = String(file.uri ?? file.url ?? '');
      const local = uri.startsWith('file://') ? decodeURIComponent(new URL(uri).pathname) : null;
      const hash = local && fs.existsSync(local) ? sha256(fs.readFileSync(local)) : null;
      seen.push({path: record.path, uri: uri.replace(/^file:\/\/.*\//, 'file://…/'), sha256: hash, staged: hash ? staged.has(hash) : null});
    }
  }
  evidence.save('image-bytes.json', seen);
  driver.check('image-bytes-sent', 'every staged image reaches OpenCode as files[] with the same SHA-256 (OCE-08)', seen.map(x => `${x.path}:${x.staged}`),
    staged.size > 0 && [...staged.keys()].every(hash => seen.some(x => x.sha256 === hash)));
}

// ------------------------------------------------------------------ list / dry run
function list() {
  for (const [id, s] of Object.entries(SCENARIOS)) {
    console.log(`${id}  ${s.title}  [${s.validation.join(', ')}]  permission=${s.permissionMode}  phases=${s.phases.join('/')}  parity=${s.parity}`);
    console.log(`    steps: ${s.steps.join(', ')}`);
  }
}

function dryRun(target, opts) {
  const ids = target === 'all' ? Object.keys(SCENARIOS) : [target];
  const nonce = 'DRYRUN00';
  for (const id of ids) {
    const s = SCENARIOS[id];
    assert.ok(s, `unknown scenario ${id}`);
    const prompts = s.prompts(nonce);
    const flat = Object.fromEntries(Object.entries(prompts).map(([k, v]) => [k, typeof v === 'function' ? [1, 2, 3].map(v) : v]));
    console.log(JSON.stringify({scenario: id, title: s.title, validation: s.validation, provider: opts.provider, tier: opts.tier ?? 'T3',
      permissionMode: s.permissionMode, phases: s.phases, steps: s.steps, prompts: flat,
      writes: ['Mutate createSpace', 'ExecuteTaskCommand createChat', 'ExecuteTaskCommand session.run | queueMessage (commandId = messageId)',
        'ExecuteTaskCommand session.respondApproval {providerChoice|enum} | session.respondInput', 'ExecuteTaskCommand session.interrupt (turn-fenced)',
        'ExecuteTaskCommand forkTask | editEarlierMessage | rewindConversation (review token)', 'CreateSideChat / SendSideChat',
        'ExecuteTaskCommand nativeReview | compactContext | setModel | steerQueuedNow | goal', 'UploadChunk / UploadCommit'].filter(w => w),
      subscriptions: ['WatchDocMessages', 'WatchSessions', 'WatchQueue', 'WatchTaskCommands', 'WatchNativeTasks'],
      unverified: s.unverified ?? []}, null, 2));
  }
}

// ------------------------------------------------------------------ self-check
// Offline checks of the recorded failure cases (QF-12..QF-21) against the
// shared plumbing; no app, no network.
async function selfCheck() {
  const results = [];
  const ok = (name, fn) => {
    try { fn(); results.push({name, passed: true}); console.error(`[PASS] ${name}`); }
    catch (error) { results.push({name, passed: false, error: String(error.message ?? error)}); console.error(`[FAIL] ${name}: ${error.message}`); }
  };
  const ocCard = {id: 'q', header: 'h', question: 'q', options: ['Deny', 'Allow once', 'Cancel turn'],
    approval: {actionKind: 'command', supportsTaskAllowance: false, details: {choices: [{label: 'Deny', scope: 'none'}, {label: 'Allow once', scope: 'request'}, {label: 'Cancel turn', scope: 'none'}]}}};
  const plainCard = {id: 'q', options: ['Allow once', 'Allow for task', 'Deny', 'Cancel'], approval: {actionKind: 'command', supportsTaskAllowance: true}};
  ok('QF-12 card choices map to providerChoice indexes', () => {
    assert.deepEqual(approvalDecision(ocCard, 'allow-once').decision, {providerChoice: {index: 1}});
    assert.deepEqual(approvalDecision(ocCard, 'deny').decision, {providerChoice: {index: 0}});
    assert.deepEqual(approvalDecision(ocCard, 'cancel').decision, {providerChoice: {index: 2}});
  });
  ok('QF-12 plain approvals use the decision enum', () => {
    assert.equal(approvalDecision(plainCard, 'allow-once').decision, 'allowOnce');
    assert.equal(approvalDecision(plainCard, 'allow-task').decision, 'allowTask');
    assert.equal(approvalDecision(plainCard, 'cancel').decision, 'cancel');
  });
  ok('QF-12 an absent choice fails instead of sending Deny', () => {
    assert.throws(() => approvalDecision(ocCard, 'allow-task'), NotAnswerable);
  });
  ok('QF-13 LUNA cross-task approvals are recognised', () => {
    assert.ok(isLunaCrossTask({approval: {actionKind: 'lunaCrossTask'}}));
    assert.ok(!isLunaCrossTask(ocCard));
  });
  ok('panel and MCP answers follow the shared contract', () => {
    assert.deepEqual(buildAnswers([{id: 'c', options: ['red', 'green', 'blue']}], {pick: 'green'}), [{questionId: 'c', labels: ['green']}]);
    assert.deepEqual(buildAnswers([{id: 'm', options: ['alpha', 'beta', 'gamma'], multiSelect: true}], {pick: ['alpha', 'gamma']}), [{questionId: 'm', labels: ['alpha', 'gamma']}]);
    const field = {id: 'f', options: [], mcp: {kind: {type: 'field', name: 'size', schema: {valueType: 'string', choices: [{value: 's', label: 'Small'}, {value: 'l', label: 'Large'}], default: 'l'}}}};
    const action = {id: 'a', options: ['Accept', 'Decline', 'Cancel'], mcp: {kind: {type: 'formAction'}}};
    assert.deepEqual(buildAnswers([field, action], {mcp: 'Accept'}), [{questionId: 'f', labels: ['Large']}, {questionId: 'a', labels: ['Accept']}]);
    assert.deepEqual(buildAnswers([field, action], {mcp: 'Decline', values: {size: 's'}}), [{questionId: 'f', labels: ['Small']}, {questionId: 'a', labels: ['Decline']}]);
  });
  ok('QF-15 transcript frames apply like apply_transcript_frame', () => {
    const e = (id, text, role = 'assistant') => ({id, role, parts: [{kind: 'text', id: 't0', text}], createdAt: 0, deviceId: 'd', status: 'streaming'});
    let state = applyTranscriptFrame([], {reset: [e('u1', 'hi', 'user')]}).entries;
    state = applyTranscriptFrame(state, {upsert: [{after: 'u1', entry: e('a1', 'he')}], append: [], remove: [], count: 2}).entries;
    state = applyTranscriptFrame(state, {upsert: [], append: [{entry: 'a1', part: 't0', text: 'llo é', len: Buffer.byteLength('hello é')}], remove: [], count: 2}).entries;
    assert.equal(state[1].parts[0].text, 'hello é');
    assert.ok(applyTranscriptFrame(state, {upsert: [], append: [{entry: 'a1', part: 't0', text: 'x', len: 3}], remove: [], count: 2}).desync);
    assert.ok(applyTranscriptFrame(state, {upsert: [{after: 'missing', entry: e('a2', 'x')}], append: [], remove: [], count: 3}).desync);
    assert.ok(applyTranscriptFrame(state, {upsert: [], append: [], remove: ['a1'], count: 2}).desync);
    assert.equal(applyTranscriptFrame(state, {upsert: [], append: [], remove: ['a1'], count: 1}).entries.length, 1);
  });
  ok('task command fences copy crates/ui/src/task_commands.rs', () => {
    const snapshot = {taskRevision: 7, bindingGeneration: 2, activeTurnId: 'turn-1'};
    const p = intent => taskCommandParams({taskId: 't', commandId: 'c', intent, snapshot, actorId: 'dev', clientSessionId: 's'});
    const run = p({kind: 'session', command: {kind: 'run', request: {}}});
    assert.deepEqual([run.expectedTaskRevision, run.expectedBindingGeneration, run.expectedTurnId], [undefined, 2, undefined]);
    const stop = p({kind: 'session', command: {kind: 'interrupt'}});
    assert.deepEqual([stop.expectedTaskRevision, stop.expectedBindingGeneration, stop.expectedTurnId], [undefined, 2, 'turn-1']);
    const fork = p({kind: 'forkTask'});
    assert.deepEqual([fork.expectedTaskRevision, fork.expectedBindingGeneration, fork.expectedTurnId], [7, 2, undefined]);
    const create = p({kind: 'createChat'});
    assert.deepEqual([create.expectedTaskRevision, create.expectedBindingGeneration], [undefined, undefined]);
    const review = p({kind: 'session', command: {kind: 'run', request: {review: {target: {type: 'uncommittedChanges'}}}}});
    assert.equal(review.expectedTaskRevision, 7);
    assert.equal(p({kind: 'steerQueuedNow'}).expectedTurnId, 'turn-1');
  });
  ok('history fork permission follows history_fork_initial_permission', () => {
    assert.equal(forkInitialPermission({permissionMode: 'full', effectivePermissionMode: 'full'}), 'full');
    assert.equal(forkInitialPermission({permissionMode: 'ask', effectivePermissionMode: 'full'}), 'auto');
    assert.equal(forkInitialPermission({permissionMode: 'ask', effectivePermissionMode: 'ask'}), 'ask');
  });
  ok('QF-18 prompts are provider-neutral and the nonce is masked', () => {
    for (const [id, s] of Object.entries(SCENARIOS)) {
      const a = JSON.stringify(s.prompts('AAAA1111'), (k, v) => (typeof v === 'function' ? v(2) : v));
      const b = JSON.stringify(s.prompts('BBBB2222'), (k, v) => (typeof v === 'function' ? v(2) : v));
      assert.equal(maskNonce(a, 'AAAA1111'), maskNonce(b, 'BBBB2222'), `${id} prompts depend on more than the nonce`);
      assert.ok(s.validation.length && s.steps.length && s.phases.length, `${id} is incomplete`);
    }
    assert.deepEqual(Object.keys(SCENARIOS).filter(id => !SCENARIOS[id].extra), ['S1', 'S2', 'S3', 'S4', 'S5', 'S6', 'S7', 'S8']);
  });
  ok('QF-20 normalization keeps kinds and drops volatile values', () => {
    const entries = [{id: 'x1', role: 'assistant', status: 'complete', parts: [
      {kind: 'reasoning', id: 'r', text: 'think'}, {kind: 'text', id: 'a', text: 'one'}, {kind: 'text', id: 'b', text: 'two'},
      {kind: 'tool', id: 'c', call: {kind: 'exec', command: 'echo SECRET-NONCE'}, resolved: true, output: 'SECRET-NONCE'},
      {kind: 'tool', id: 'd', call: {kind: 'unknown', name: 'Agent: explore the repo'}, subagentRef: 'chat--sub--1', subagentStatus: 'completed'}]}];
    const normalized = JSON.stringify(normalizeEntries(entries));
    assert.ok(!normalized.includes('SECRET-NONCE') && !normalized.includes('x1') && !normalized.includes('explore'));
    assert.deepEqual(normalizeEntries(entries)[0].parts.map(p => p.tool ?? p.kind), ['reasoning', 'text', 'exec', 'unknown:Agent']);
  });
  ok('the diff tool reports differences and honours reasoned allowances only', () => {
    const diff = spawnSync(process.execPath, [path.join(root, 'scripts/qa-opencode-diff.mjs'), 'self-check'], {encoding: 'utf8'});
    assert.equal(diff.status, 0, diff.stderr || diff.stdout);
  });
  ok('a solid PNG fixture decodes to its colour', () => {
    const png = solidPng(4, 4, [220, 20, 20]);
    assert.equal(png.subarray(1, 4).toString('ascii'), 'PNG');
  });
  const passed = results.every(r => r.passed);
  console.log(JSON.stringify({selfCheck: passed ? 'passed' : 'failed', checks: results.length, failed: results.filter(r => !r.passed)}));
  return passed;
}

// ------------------------------------------------------------------ rehearse
// Rehearsal fixtures are disposable: remove exactly the temp parents the
// runs recorded (QF-02 prefix, system temp dir), never anything else.
function removeRehearsalFixtures(runDirs) {
  for (const dir of runDirs) {
    const fixture = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8')).fixture;
    const parent = fixture ? path.dirname(fixture) : null;
    if (parent && path.basename(parent).startsWith('luna-qa-opencode-scenario-') && path.dirname(fs.realpathSync(parent)) === fs.realpathSync(os.tmpdir())) {
      fs.rmSync(parent, {recursive: true, force: true});
    }
  }
}

// QF-21: the whole runner against a fake engine this process spawned. Proves
// the plumbing (RPC shapes, waits, evidence, diff), never LUNA behaviour.
async function rehearse(target, opts) {
  const ids = !target || target === 'all' ? Object.keys(SCENARIOS).filter(id => !SCENARIOS[id].extra) : [target];
  assert.ok(ids.every(id => SCENARIOS[id] && !SCENARIOS[id].extra), 'live-only extra scenarios are not rehearsed (no fake background children, servers or mock)');
  const out = path.resolve(opts['evidence-root'] ?? path.join(root, 'target/qa/opencode-rehearsal'));
  const summary = [];
  const fakeArgs = opts['fake-args'] ? opts['fake-args'].split(' ') : [];
  const runDirs = new Set();
  let engine = await spawnFakeEngine(fakeArgs);
  try {
    for (const id of ids) {
      const runs = {};
      for (const provider of PROVIDERS) {
        const base = {...opts, provider, 'evidence-root': out, 'turn-timeout': opts['turn-timeout'] ?? '20'};
        if (SCENARIOS[id].phases.includes('pre')) {
          const pre = await runScenario(id, {...base, phase: 'pre'}, {rehearsal: engine});
          // A "restarted app": the fake saves its state and exits; a new
          // process (new pid) loads it. The runner itself never restarts apps.
          const {state} = await (await fetch(`http://127.0.0.1:${engine.port}/__fake/restart`, {method: 'POST'})).json();
          await new Promise(resolve => engine.child.once('exit', resolve));
          engine = await spawnFakeEngine([...fakeArgs, '--state', state]);
          runs[provider] = await runScenario(id, {...base, phase: 'post', run: pre.evidence}, {rehearsal: engine});
          fs.rmSync(state, {force: true});
        } else {
          runs[provider] = await runScenario(id, base, {rehearsal: engine});
        }
      }
      for (const run of Object.values(runs)) runDirs.add(run.evidence);
      const diff = spawnSync(process.execPath, [path.join(root, 'scripts/qa-opencode-diff.mjs'), 'compare', runs.opencode.evidence, runs['claude-code'].evidence,
        '--out', path.join(out, id.toLowerCase(), 'parity-diff')], {encoding: 'utf8'});
      summary.push({scenario: id, opencode: runs.opencode.passed, claude: runs['claude-code'].passed, diffExit: diff.status, diff: diff.stdout.trim().split('\n').at(-1)});
    }
  } finally {
    engine.child.kill('SIGTERM');
    removeRehearsalFixtures(runDirs);
  }
  fs.mkdirSync(out, {recursive: true});
  fs.writeFileSync(path.join(out, 'rehearsal-summary.json'), `${JSON.stringify({tier: 'R0', note: 'fake engine; driver plumbing only (QF-21)', at: now(), summary}, null, 2)}\n`);
  console.log(JSON.stringify({rehearsal: summary}, null, 2));
  return summary.every(s => s.opencode && s.claude && [0, 1].includes(s.diffExit));
}

// QF-17 end to end: a lost ExecuteTaskCommand reply is read back by command
// id (no resend); a command whose outcome stays unknown stops the run with
// no further writes. Fake engine only (R0).
async function rehearseFaults(opts) {
  const out = path.resolve(opts['evidence-root'] ?? path.join(root, 'target/qa/opencode-rehearsal'), 'faults');
  const results = [];
  const runWith = async (fakeArgs, extra) => {
    const engine = await spawnFakeEngine(fakeArgs);
    let result;
    try {
      result = await runScenario('S6', {...opts, provider: 'opencode', 'evidence-root': out, 'turn-timeout': '20', ...extra}, {rehearsal: engine});
      return result;
    } finally {
      engine.child.kill('SIGTERM');
      if (result) removeRehearsalFixtures([result.evidence]);
    }
  };
  const lost = await runWith(['--drop-reply', 'session'], {});
  const lostManifest = JSON.parse(fs.readFileSync(path.join(lost.evidence, 'manifest.json'), 'utf8'));
  const readback = lostManifest.sends.filter(send => send.readback);
  const sameIds = new Set(lostManifest.sends.map(send => send.commandId)).size === lostManifest.sends.length;
  results.push({name: 'lost reply is read back once, never resent', passed: lost.passed && readback.length === 1 && sameIds,
    detail: {passed: lost.passed, readback: readback.map(send => send.commandId), sends: lostManifest.sends.length}});
  const unknown = await runWith(['--lose-command', 'session'], {'rpc-timeout': '2000'});
  const unknownManifest = JSON.parse(fs.readFileSync(path.join(unknown.evidence, 'manifest.json'), 'utf8'));
  const index = unknownManifest.sends.findIndex(send => send.outcome === 'unknown');
  const later = index < 0 ? [] : unknownManifest.sends.slice(index + 1);
  results.push({name: 'unknown outcome stops the run with no later writes', passed: !unknown.passed && index >= 0 && later.length === 0
    && (unknownManifest.phases.run.cleanup ?? []).every(row => row.skipped),
  detail: {passed: unknown.passed, unknownIndex: index, laterSends: later.length, cleanup: unknownManifest.phases.run.cleanup}});
  fs.mkdirSync(out, {recursive: true});
  fs.writeFileSync(path.join(out, 'faults-summary.json'), `${JSON.stringify({tier: 'R0', at: now(), results}, null, 2)}\n`);
  for (const r of results) console.error(`[${r.passed ? 'PASS' : 'FAIL'}] ${r.name}: ${JSON.stringify(r.detail)}`);
  return results.every(r => r.passed);
}

// ------------------------------------------------------------------ main
const mode = process.argv[2];
const {values: opts, positionals} = parseArgs({
  args: process.argv.slice(3),
  options: {
    provider: {type: 'string', default: 'opencode'}, tier: {type: 'string'}, model: {type: 'string'}, variant: {type: 'string'},
    'evidence-root': {type: 'string'}, 'turn-timeout': {type: 'string'}, 'allow-usage-store': {type: 'boolean', default: false},
    phase: {type: 'string'}, run: {type: 'string'}, 'fake-args': {type: 'string'}, 'rpc-timeout': {type: 'string'}, ax: {type: 'boolean', default: false},
    'hold-ms': {type: 'string'}, 'capture-hold-ms': {type: 'string'}, 'stall-wait-ms': {type: 'string'}, 'unattended-ms': {type: 'string'}, 'big-command': {type: 'string'}, 'mock-url': {type: 'string'},
  },
  allowPositionals: true,
});
const target = positionals[0];
let exitCode = 0;
if (mode === 'list') list();
else if (mode === 'dry-run') dryRun(target ?? 'all', opts);
else if (mode === 'self-check') exitCode = (await selfCheck()) ? 0 : 1;
else if (mode === 'rehearse') exitCode = (await rehearse(target, opts)) ? 0 : 1;
else if (mode === 'rehearse-faults') exitCode = (await rehearseFaults(opts)) ? 0 : 1;
else if (mode === 'run') {
  assert.ok(target && SCENARIOS[target], `run needs a scenario id: ${Object.keys(SCENARIOS).join(', ')}`);
  const result = await runScenario(target, opts);
  exitCode = result.passed ? 0 : 1;
} else {
  console.error('usage: qa-opencode-scenarios.mjs list | dry-run <S#|all> | self-check | rehearse [<S#|all>] | rehearse-faults | run <S#> --provider P --tier T (see header)');
  exitCode = 64;
}
process.exit(exitCode);
