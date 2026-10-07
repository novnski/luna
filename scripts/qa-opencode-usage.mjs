#!/usr/bin/env node
// OpenCode usage readback for a QA batch (S6, OCE-13): the QA app's own
// GetUsageSnapshot (OpenCode history and the OpenCode Go limits card) next to
// `opencode stats --models --cost` run against the SAME isolated profile.
//
//   node scripts/qa-opencode-usage.mjs --out <dir> --label <name> [--days 1] [--no-stats]
//
// Writes <dir>/go-usage-<label>.json (OpenCode-only extract: buckets, sources,
// limits and the provider labels; Claude and Codex rows are reduced to their
// provider names) and <dir>/opencode-stats-<label>.txt. The identity checks of
// the scenario runner run first (QF-09). The stats command goes through the
// profile wrapper, which keeps it off OpenCode's shared service (QF-25); a
// `serve --service` process seen during the run is recorded as a failure.
// Read-only: no send, no answer, no model call.
import fs from 'node:fs';
import path from 'node:path';
import {spawn} from 'node:child_process';
import {execFileSync} from 'node:child_process';
import {parseArgs} from 'node:util';
import {Rpc, assertQaIdentity, now} from './qa-opencode-lib.mjs';

const {values: opt} = parseArgs({options: {
  out: {type: 'string'}, label: {type: 'string', default: 'readback'}, days: {type: 'string', default: '1'},
  'no-stats': {type: 'boolean', default: false},
}});
if (!opt.out) { console.error('usage: qa-opencode-usage.mjs --out <dir> --label <name> [--days 1] [--no-stats]'); process.exit(64); }
const out = path.resolve(opt.out);
fs.mkdirSync(out, {recursive: true});
const identity = assertQaIdentity({provider: 'opencode'});
const OPENCODE = new Set(['openCodeGo', 'opencode', 'openCode']);

const rpc = await new Rpc({port: identity.port, log: () => {}}).connect();
let snapshot;
try {
  snapshot = await rpc.call('GetUsageSnapshot', {days: Number(opt.days), includeLimits: true}, {timeoutMs: 180000});
} finally {
  rpc.close?.();
}
const summary = snapshot?.summary ?? {};
const extract = {
  readAt: now(), readAtMs: summary.readAtMs ?? null, appPid: identity.pid, days: Number(opt.days),
  opencodeBuckets: (summary.buckets ?? []).filter(b => OPENCODE.has(b.provider)),
  opencodeSources: (summary.sources ?? []).filter(s => OPENCODE.has(s.provider)),
  opencodeLimits: (snapshot?.limits ?? []).filter(l => OPENCODE.has(l.provider)).map(l => ({...l, verifiedAccountId: l.verifiedAccountId ? '<present>' : null})),
  opencodeWarnings: (summary.warnings ?? []).filter(w => /opencode/i.test(JSON.stringify(w))),
  providersInSnapshot: [...new Set((summary.buckets ?? []).map(b => b.provider))],
  limitProviders: (snapshot?.limits ?? []).map(l => l.provider),
};
const totals = {};
for (const b of extract.opencodeBuckets) {
  const t = (totals[b.model] ??= {records: 0, uncachedInput: 0, cachedInput: 0, cacheCreation: 0, output: 0, reasoning: 0, knownCostUsd: 0});
  t.records += b.records ?? 0;
  for (const k of ['uncachedInput', 'cachedInput', 'cacheCreation', 'output', 'reasoning']) t[k] += b.tokens?.[k] ?? 0;
  t.knownCostUsd += b.knownCostUsd ?? 0;
}
extract.opencodeTotalsByModel = totals;
fs.writeFileSync(path.join(out, `go-usage-${opt.label}.json`), JSON.stringify(extract, null, 2) + '\n');

let stats = null;
if (!opt['no-stats']) {
  const services = new Set();
  const watch = setInterval(() => {
    try {
      for (const line of execFileSync('ps', ['-axo', 'pid,command'], {encoding: 'utf8'}).split('\n')) {
        if (/opencode serve --service/.test(line)) services.add(line.trim());
      }
    } catch {}
  }, 200);
  const result = await new Promise(resolve => {
    const child = spawn(identity.profile.wrapper, ['stats', '--models', '--cost'], {cwd: out, stdio: ['ignore', 'pipe', 'pipe']});
    let stdout = '', stderr = '';
    child.stdout.on('data', d => { stdout += d; });
    child.stderr.on('data', d => { stderr += d; });
    const timer = setTimeout(() => child.kill('SIGTERM'), 120000);
    child.on('close', code => { clearTimeout(timer); resolve({code, stdout, stderr}); });
  });
  clearInterval(watch);
  stats = {exit: result.code, serviceProcesses: [...services]};
  fs.writeFileSync(path.join(out, `opencode-stats-${opt.label}.txt`),
    `# ${identity.profile.wrapper} stats --models --cost (cwd ${out}) at ${now()}\n# exit ${result.code}; serve --service seen: ${services.size}\n${result.stdout}${result.stderr ? `\n# stderr\n${result.stderr}` : ''}`);
}
const limit = extract.opencodeLimits[0] ?? null;
console.log(JSON.stringify({
  label: opt.label,
  limits: limit && {availability: limit.availability, plan: limit.plan, detail: limit.detail, windows: (limit.windows ?? []).map(w => `${w.label}: ${w.usedPercent}%`)},
  totals, stats,
}, null, 2));
process.exit(stats && stats.serviceProcesses.length ? 1 : 0);
