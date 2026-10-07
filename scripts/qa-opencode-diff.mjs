#!/usr/bin/env node
// Normalized-artifact diff for PD-14 parity runs (validation-plan §1 item 8):
// the same scenario run against OpenCode and Claude Code by
// scripts/qa-opencode-scenarios.mjs, compared step by step.
//
//   node scripts/qa-opencode-diff.mjs compare <opencode-run> <claude-run>
//        [--allow <allow.json>] [--out <dir>]
//   node scripts/qa-opencode-diff.mjs template <diff.json> [<allow.json>]
//   node scripts/qa-opencode-diff.mjs self-check
//
// Compared per step (normalized/steps-*.json): document layout per chat role
// (part kinds, tool kinds and flags, input/approval/question shapes), the tool
// multiset, session statuses, queue and native-task rows, context control,
// receipts (intent, status, reason code), approval and question payload
// shapes and side-chat modes. Volatile values (ids, timestamps, nonces, prose,
// paths) never reach the normalized files (QF-20).
//
// Refusals (exit 2, QF-18): different scenarios, different step lists, or
// different masked prompt hashes per step. Allowance entries
// ({"step": "<label>|*", "path": "<prefix>", "reason": "..."}) need a reason
// and only mark a listed difference as an expected native difference; they
// never remove it from the report. Exit 0: no unexplained difference;
// 1: unexplained differences.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {parseArgs} from 'node:util';

const PHASES = ['pre', 'run', 'post'];

function loadRun(dir) {
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
  const steps = [];
  for (const phase of PHASES) {
    const file = path.join(dir, 'normalized', `steps-${phase}.json`);
    if (fs.existsSync(file)) steps.push(...JSON.parse(fs.readFileSync(file, 'utf8')).map(step => ({...step, phase})));
  }
  return {dir, manifest, steps};
}

const partForm = part => {
  if (part.kind === 'tool') return `tool:${part.tool}${part.resolved ? '' : '~unresolved'}${part.isError ? '!error' : ''}${part.subagent ? `@sub:${part.subagent}` : ''}`;
  if (part.kind === 'input') return `input[${part.questions.map(q => q.approval ? `approval:${q.approval.actionKind}` : q.mcp ? `mcp:${q.mcp.kind}` : `question:${q.optionCount}${q.multiSelect ? ':multi' : ''}`).join(',')}]${part.resolved ? '' : '~pending'}`;
  return part.kind;
};
export const layoutForm = layout => (layout ?? []).map(entry => `${entry.role}${entry.status && entry.status !== 'complete' ? `(${entry.status})` : ''}: ${entry.parts.map(partForm).join(' ') || '-'}`);

// Prompt hashes per step (nonce masked by the runner), sorted.
function promptHashes(manifest) {
  const byStep = {};
  for (const send of manifest.sends ?? []) {
    if (!send.promptSha256) continue;
    (byStep[send.step ?? '-'] ??= []).push(send.promptSha256);
  }
  for (const list of Object.values(byStep)) list.sort();
  return byStep;
}

function stepView(step) {
  const o = step.observations ?? {};
  const view = {};
  for (const [role, chat] of Object.entries(o.chats ?? {})) {
    view[`chats.${role}.layout`] = layoutForm(chat.layout);
    view[`chats.${role}.tools`] = chat.tools ?? {};
    view[`chats.${role}.statuses`] = [...(chat.statuses ?? [])].sort();
    view[`chats.${role}.pendingInputs`] = chat.pendingInputs ?? 0;
    view[`chats.${role}.queueMax`] = chat.queueMax ?? 0;
    view[`chats.${role}.nativeTasks`] = chat.nativeTasks ?? [];
    view[`chats.${role}.contextControl`] = chat.contextControl ?? null;
  }
  view.receipts = (o.receipts ?? []).map(r => `${r.intent}${r.control ? `/${r.control}` : ''}:${r.status}${r.reasonCode ? `:${r.reasonCode}` : ''}`);
  view.approvals = (o.approvals ?? []).map(a => ({action: a.action, choice: a.choice, shape: a.shape}));
  view.questions = (o.questions ?? []).map(q => ({shapes: q.shapes, answered: q.answered}));
  view.inputs = (o.inputs ?? []).map(i => i.questions);
  view.sideChats = (o.sideChats ?? []).map(sc => ({role: sc.role, outcome: sc.outcome, contextMode: sc.contextMode ?? null}));
  view.ok = Boolean(step.ok);
  return view;
}

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

export function compareRuns(a, b, allow = []) {
  for (const entry of allow) {
    assert.ok(entry && typeof entry.reason === 'string' && entry.reason.trim().length >= 8, `allowance ${JSON.stringify(entry)} needs a reason (QF-20)`);
    assert.ok(!/^\s*TODO/i.test(entry.reason), `allowance ${JSON.stringify(entry)} still has a TODO reason (QF-20)`);
    assert.ok(typeof entry.path === 'string' && entry.path.length, `allowance ${JSON.stringify(entry)} needs a path`);
  }
  const refusals = [];
  if (a.manifest.scenario !== b.manifest.scenario) refusals.push(`different scenarios: ${a.manifest.scenario} vs ${b.manifest.scenario}`);
  const labelsA = a.steps.map(s => `${s.phase}:${s.label}`);
  const labelsB = b.steps.map(s => `${s.phase}:${s.label}`);
  if (!same(labelsA, labelsB)) refusals.push(`different step lists: [${labelsA.join(', ')}] vs [${labelsB.join(', ')}]`);
  const hashesA = promptHashes(a.manifest);
  const hashesB = promptHashes(b.manifest);
  if (!same(hashesA, hashesB)) refusals.push('different masked prompt hashes per step (QF-18): the runs did not send the same prompts');
  if (refusals.length) return {refused: refusals, differences: []};
  const differences = [];
  for (const [index, stepA] of a.steps.entries()) {
    const stepB = b.steps[index];
    const viewA = stepView(stepA);
    const viewB = stepView(stepB);
    for (const key of [...new Set([...Object.keys(viewA), ...Object.keys(viewB)])].sort()) {
      if (same(viewA[key], viewB[key])) continue;
      const allowed = allow.find(entry => (entry.step === '*' || entry.step === stepA.label) && key.startsWith(entry.path));
      differences.push({step: stepA.label, phase: stepA.phase, path: key, a: viewA[key] ?? null, b: viewB[key] ?? null, allowed: allowed ? {reason: allowed.reason} : false});
    }
  }
  return {refused: null, differences};
}

function writeReport(out, a, b, result) {
  fs.mkdirSync(out, {recursive: true});
  const report = {
    scenario: a.manifest.scenario, comparedAt: new Date().toISOString(),
    a: {dir: a.dir, provider: a.manifest.requested?.provider, model: a.manifest.requested?.model, tier: a.manifest.tier, passed: a.manifest.passed ?? null},
    b: {dir: b.dir, provider: b.manifest.requested?.provider, model: b.manifest.requested?.model, tier: b.manifest.tier, passed: b.manifest.passed ?? null},
    refused: result.refused, differences: result.differences,
    counts: {total: result.differences.length, allowed: result.differences.filter(d => d.allowed).length, unexplained: result.differences.filter(d => !d.allowed).length},
  };
  fs.writeFileSync(path.join(out, 'diff.json'), `${JSON.stringify(report, null, 2)}\n`);
  const cell = value => JSON.stringify(value).replaceAll('|', '/').slice(0, 400);
  fs.writeFileSync(path.join(out, 'diff.md'), [
    `# Parity diff: ${report.scenario}`, '',
    `- A: ${report.a.provider} ${report.a.model ?? ''} (${report.a.tier}) ${report.a.dir}`,
    `- B: ${report.b.provider} ${report.b.model ?? ''} (${report.b.tier}) ${report.b.dir}`,
    result.refused ? `- **Refused:** ${result.refused.join('; ')}` : `- Differences: ${report.counts.total} (${report.counts.allowed} expected native differences, **${report.counts.unexplained} unexplained**)`, '',
    ...(result.refused ? [] : ['| Step | Path | A | B | Expected? |', '| --- | --- | --- | --- | --- |',
      ...result.differences.map(d => `| ${d.phase}:${d.step} | ${d.path} | ${cell(d.a)} | ${cell(d.b)} | ${d.allowed ? d.allowed.reason.replaceAll('|', '/') : 'NO'} |`)]),
    '', 'Each expected difference must also be recorded in the OpenCode run\'s result.md (validation-plan §1 item 8).', '',
  ].join('\n'));
  return report;
}

// Writes an allowance skeleton for a diff.json: one entry per unexplained
// difference with a TODO reason that `compare` refuses until a person
// replaces it with the native difference it records.
function templateMode(args) {
  const [file, out] = args;
  assert.ok(file, 'template needs <diff.json> [<allow.json>]');
  const report = JSON.parse(fs.readFileSync(file, 'utf8'));
  const seen = new Set();
  const entries = [];
  for (const d of report.differences ?? []) {
    if (d.allowed || seen.has(`${d.step}|${d.path}`)) continue;
    seen.add(`${d.step}|${d.path}`);
    entries.push({step: d.step, path: d.path, reason: 'TODO: name the native OpenCode behaviour that makes this differ, or fix the parity gap'});
  }
  const target = path.resolve(out ?? path.join(path.dirname(file), 'allow-template.json'));
  fs.writeFileSync(target, `${JSON.stringify(entries, null, 2)}\n`);
  console.log(JSON.stringify({template: target, entries: entries.length}));
  return 0;
}

function compareMode(args) {
  const {values, positionals} = parseArgs({args, options: {allow: {type: 'string'}, out: {type: 'string'}}, allowPositionals: true});
  assert.equal(positionals.length, 2, 'compare needs <run-a> <run-b>');
  const [a, b] = positionals.map(dir => loadRun(path.resolve(dir)));
  const allow = values.allow ? JSON.parse(fs.readFileSync(values.allow, 'utf8')) : [];
  let result;
  try {
    result = compareRuns(a, b, allow);
  } catch (error) {
    result = {refused: [String(error.message ?? error)], differences: []};
  }
  const out = path.resolve(values.out ?? path.join(a.dir, `parity-diff-vs-${b.manifest.requested?.provider ?? 'b'}`));
  const report = writeReport(out, a, b, result);
  console.log(JSON.stringify({out, refused: result.refused, ...report.counts}));
  return result.refused ? 2 : report.counts.unexplained ? 1 : 0;
}

// Offline check of the comparison rules on synthetic runs.
function selfCheck() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'luna-qa-diff-'));
  const layout = kinds => [{role: 'user', status: null, parts: [{kind: 'text'}]}, {role: 'assistant', status: 'complete', parts: kinds}];
  const write = (name, {provider, steps, hashes = ['h1'], scenario = 'S1'}) => {
    const dir = path.join(tmp, name);
    fs.mkdirSync(path.join(dir, 'normalized'), {recursive: true});
    fs.writeFileSync(path.join(dir, 'manifest.json'), JSON.stringify({scenario, tier: 'T3', requested: {provider}, sends: hashes.map(h => ({step: 'allow-once', promptSha256: h}))}));
    fs.writeFileSync(path.join(dir, 'normalized', 'steps-run.json'), JSON.stringify(steps));
    return loadRun(dir);
  };
  const step = (kinds, receipts = [{intent: 'session', control: 'respondApproval', status: 'applied'}]) => [{label: 'allow-once', ok: true, observations: {chats: {root: {layout: layout(kinds), tools: {exec: 1}, statuses: ['idle', 'working']}}, receipts}}];
  const base = [{kind: 'tool', tool: 'exec', resolved: true, isError: false}, {kind: 'text'}];
  const results = [];
  const expect = (name, fn) => { try { fn(); results.push(name); } catch (error) { console.error(`[FAIL] ${name}: ${error.message}`); throw error; } };
  expect('identical runs have no differences', () => {
    const r = compareRuns(write('a1', {provider: 'opencode', steps: step(base)}), write('b1', {provider: 'claude-code', steps: step(base)}));
    assert.equal(r.refused, null);
    assert.equal(r.differences.length, 0);
  });
  expect('a layout difference is reported', () => {
    const r = compareRuns(write('a2', {provider: 'opencode', steps: step([{kind: 'reasoning'}, ...base])}), write('b2', {provider: 'claude-code', steps: step(base)}));
    assert.deepEqual(r.differences.map(d => d.path), ['chats.root.layout']);
    assert.equal(r.differences[0].allowed, false);
  });
  expect('a reasoned allowance marks but keeps the difference', () => {
    const r = compareRuns(write('a3', {provider: 'opencode', steps: step([{kind: 'reasoning'}, ...base])}), write('b3', {provider: 'claude-code', steps: step(base)}),
      [{step: '*', path: 'chats.root.layout', reason: 'OpenCode streams reasoning parts natively'}]);
    assert.equal(r.differences.length, 1);
    assert.ok(r.differences[0].allowed.reason);
  });
  expect('an allowance without a reason is refused', () => {
    assert.throws(() => compareRuns(write('a4', {provider: 'opencode', steps: step(base)}), write('b4', {provider: 'claude-code', steps: step(base)}), [{step: '*', path: 'chats'}]));
  });
  expect('a TODO reason is refused', () => {
    assert.throws(() => compareRuns(write('a8', {provider: 'opencode', steps: step(base)}), write('b8', {provider: 'claude-code', steps: step(base)}), [{step: '*', path: 'chats', reason: 'TODO: explain this later'}]));
  });
  expect('different prompts are refused (QF-18)', () => {
    const r = compareRuns(write('a5', {provider: 'opencode', steps: step(base), hashes: ['h1']}), write('b5', {provider: 'claude-code', steps: step(base), hashes: ['h2']}));
    assert.ok(r.refused?.length);
  });
  expect('different scenarios are refused', () => {
    const r = compareRuns(write('a6', {provider: 'opencode', steps: step(base)}), write('b6', {provider: 'claude-code', steps: step(base), scenario: 'S2'}));
    assert.ok(r.refused?.some(x => x.includes('scenarios')));
  });
  expect('receipt reason codes are compared', () => {
    const r = compareRuns(write('a7', {provider: 'opencode', steps: step(base, [{intent: 'session', control: 'respondApproval', status: 'rejected', reasonCode: 'staleTurn'}])}), write('b7', {provider: 'claude-code', steps: step(base)}));
    assert.deepEqual(r.differences.map(d => d.path), ['receipts']);
  });
  fs.rmSync(tmp, {recursive: true, force: true});
  console.log(JSON.stringify({selfCheck: 'passed', checks: results.length}));
  return 0;
}

const [mode, ...rest] = process.argv.slice(2);
if (mode === 'compare') process.exit(compareMode(rest));
else if (mode === 'self-check') process.exit(selfCheck());
else if (mode === 'template') process.exit(templateMode(rest));
else {
  console.error('usage: qa-opencode-diff.mjs compare <run-a> <run-b> [--allow allow.json] [--out dir] | template <diff.json> [allow.json] | self-check');
  process.exit(64);
}
