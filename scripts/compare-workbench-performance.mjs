#!/usr/bin/env node
// Compare three matched before/after runs. Retain the JSON even on failure.
// Usage: compare-workbench-performance.mjs ABSOLUTE_EVIDENCE_ROOT
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const root = process.argv[2];
assert.ok(root && path.isAbsolute(root), 'absolute evidence root required');
const read = name => JSON.parse(fs.readFileSync(path.join(root, name), 'utf8'));
const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
const repeats = [1, 2, 3];
const selectionPath = path.join(root, 'native-candidate-selection.json');
const selection = fs.existsSync(selectionPath) ? read('native-candidate-selection.json') : null;
const sumItems = (counters, name) => Object.values(counters[name] ?? {}).reduce((n, v) => n + v.items, 0);
const result = {passed: true, host: [], native: [], checks: []};
function check(name, observed, limit, operator = '<=') {
  assert.ok(Number.isFinite(observed) && Number.isFinite(limit), `${name}: missing measurement`);
  const passed = operator === '===' ? observed === limit : observed <= limit;
  result.checks.push({name, observed, operator, limit, passed});
  result.passed &&= passed;
}

function hostRuns(prefix, entries) {
  return repeats.map(run => {
    const data = read(`${prefix}-${run}/host.json`);
    assert.equal(data.passed, true);
    const w = data.workloads.find(w => w.historyEntries === entries);
    assert.ok(w && w.sameIdSameLengthTextAndToolRewrite && w.rewindNoStaleSuffix);
    const memory = w.reopenAfterRewrite.memory.map(m => m.memory.rssMiB);
    return {
      metadataCpuMs: w.metadataUpdates.cpuMs,
      streamingCpuMs: w.streamingWithHiddenChat.cpuMs,
      coldMs: w.cold.wallMs,
      reopenP50Ms: w.reopenAfterRewrite.timing.p50Ms,
      reusedProjections: w.metadataUpdates.reusedProjections,
      metadataBytes: w.metadataUpdates.payloadBytes,
      streamingBytes: w.streamingWithHiddenChat.payloadBytes,
      outputSha256: w.fullOutputSha256,
      navigationGrowthMiB: Math.max(...memory) - memory[0],
      warmGrowthMiB: Math.max(...memory.slice(1)) - memory[1],
    };
  });
}

function nativeRuns(prefix, entries, selected = repeats) {
  assert.ok(selected.length === 3 && new Set(selected).size === 3 && selected.every(n => Number.isSafeInteger(n) && n > 0), 'three distinct run numbers required');
  return selected.map(run => {
    const dir = `${prefix}-${entries}-${run}`;
    const data = read(`${dir}/results.json`);
    const identity = read(`${dir}/process-manifest.json`);
    const timings = read(`${dir}/timing-validation.json`);
    const process = read(`${dir}/process-phases.json`).phases;
    const candidate = prefix.startsWith('ui-candidate');
    assert.equal(identity.mode, candidate ? 'candidate' : 'baseline');
    if (candidate) {
      assert.equal(data.layoutProbesWhileMeasuring, false, 'extra layout probes invalidate matched timings');
      assert.equal(timings.scope, 'matched-benchmark-phases');
      assert.equal(read(`${dir}/startup-activation.json`).active, true);
      assert.equal(data.phases.reopen.cycles.length, 20);
      for (const cycle of data.phases.reopen.cycles) {
        for (const key of ['awayFrames', 'backFrames']) {
          const frame = cycle[key];
          assert.ok(frame && frame.windowActiveSamples > 0 && frame.inactiveWindowSamples === 0 && frame.missingFrameSamples === 0,
            'each candidate reopen must retain complete foreground frame observations');
        }
      }
    }
    assert.ok(identity.signatureValid && identity.binaryUnchanged && identity.runtimeVerified);
    assert.equal(identity.outcome.code, 0);
    assert.ok(timings.observations > 0 && timings.inactive === 0 && timings.missing === 0);
    assert.equal(data.entries, entries);
    const typingChecks = data.checks.filter(c => c.name.includes(': 60 keystrokes reach Main\'s draft'));
    assert.equal(typingChecks.length, 3, 'one typing readback per pane count');
    for (const c of typingChecks) {
      assert.ok(c.pass && c.detail.draftLen === 60, 'typing workload was incomplete or received extra input');
    }
    const row = {
      evidenceDirectory: dir,
      binarySha256: identity.binarySha256,
      fixtureSourceSha256: identity.fixtureSourceSha256,
      failedChecks: data.failedChecks.length,
      coldMs: data.phases.coldOpen.open.selectToTailPaintedMs,
      reopenP50Ms: data.phases.reopen.backMs.p50,
      navigationGrowthMiB: process.reopen.rssEndMiB - process.reopen.rssStartMiB,
      // Later correctness-only phases may deliberately allocate oversize
      // inputs. Compare memory over the phases present in both executables.
      peakFootprintMiB: Math.max(...[
        'cold-open', 'reopen', 'panes-1', 'panes-2', 'panes-4',
        'child-updates', 'rewrites', 'ownership',
      ].map(name => process[name].footprintPeakMiB)),
    };
    for (const groups of [1, 2, 4]) {
      const phase = data.phases[`panes${groups}`];
      assert.equal(phase.typing.inputToPresentedMs.count, 60);
      row[`typing${groups}P95Ms`] = phase.typing.inputToPresentedMs.p95;
      row[`frames${groups}P95Ms`] = phase.frameIntervalsMs.p95;
      row[`cpu${groups}Ms`] = process[`panes-${groups}`].sampledCpuMs;
      row[`fingerprint${groups}Items`] = sumItems(phase.typingCounters, 'transcript.fingerprint');
      row[`composer${groups}Items`] = sumItems(phase.typingCounters, 'composer.pending_scan');
    }
    return row;
  });
}

for (const entries of [1200, 4200]) {
  const before = hostRuns('host-baseline-mimalloc', entries);
  const after = hostRuns(selection?.hostPrefix ?? 'host-candidate', entries);
  result.host.push({entries, before, after});
  const b = key => median(before.map(r => r[key]));
  const a = key => median(after.map(r => r[key]));
  check(`host ${entries}: metadata CPU reduction >=85%`, a('metadataCpuMs'), b('metadataCpuMs') * 0.15);
  check(`host ${entries}: streaming CPU reduction >=50%`, a('streamingCpuMs'), b('streamingCpuMs') * 0.5);
  check(`host ${entries}: cold latency`, a('coldMs'), b('coldMs') * 1.2);
  check(`host ${entries}: warm latency`, a('reopenP50Ms'), b('reopenP50Ms') * 1.2);
  check(`host ${entries}: navigation RSS growth`, Math.max(...after.map(r => r.navigationGrowthMiB)), 50);
  check(`host ${entries}: later navigation RSS growth`, Math.max(...after.map(r => r.warmGrowthMiB)), 20);
  after.forEach((r, index) => {
    check(`host ${entries} run ${index + 1}: reused projections`, r.reusedProjections, 100, '===');
    for (const key of ['metadataBytes', 'streamingBytes']) {
      check(`host ${entries} run ${index + 1}: ${key}`, r[key], before[index][key], '===');
    }
    assert.equal(r.outputSha256, before[index].outputSha256, 'durable tool output bytes changed');
  });

  const nativeBefore = nativeRuns('ui-baseline-final', entries);
  const nativeAfter = nativeRuns(selection?.prefix ?? 'ui-candidate-verified', entries, selection?.runs?.[entries] ?? repeats);
  result.native.push({entries, before: nativeBefore, after: nativeAfter});
  for (const rows of [nativeBefore, nativeAfter]) {
    assert.equal(new Set(rows.map(r => r.binarySha256)).size, 1, 'three repeats must use one executable');
    assert.equal(new Set(rows.map(r => r.fixtureSourceSha256)).size, 1, 'fixture changed between repeats');
  }
  const nb = key => median(nativeBefore.map(r => r[key]));
  const na = key => median(nativeAfter.map(r => r[key]));
  check(`native ${entries}: correctness failures`, Math.max(...nativeAfter.map(r => r.failedChecks)), 0, '===');
  check(`native ${entries}: cold latency`, na('coldMs'), nb('coldMs') * 1.2);
  check(`native ${entries}: reopen improvement >=25%`, na('reopenP50Ms'), nb('reopenP50Ms') * 0.75);
  for (const key of ['navigationGrowthMiB', 'peakFootprintMiB']) {
    check(`native ${entries}: ${key}`, na(key), Math.max(nb(key) * 1.2, nb(key) + 32));
  }
  for (const groups of [1, 2, 4]) {
    for (const key of [`typing${groups}P95Ms`, `frames${groups}P95Ms`]) {
      check(`native ${entries}: ${key}`, na(key), nb(key) * 1.25 + 2);
    }
    check(`native ${entries}: ${groups}-pane CPU reduction >=30%`, na(`cpu${groups}Ms`), nb(`cpu${groups}Ms`) * 0.7);
    for (const key of [`fingerprint${groups}Items`, `composer${groups}Items`]) {
      check(`native ${entries}: ${key} reduction >=90%`, na(key), nb(key) * 0.1);
    }
  }
}

fs.writeFileSync(path.join(root, 'comparison.json'), `${JSON.stringify(result, null, 2)}\n`);
console.log(JSON.stringify({passed: result.passed, checks: result.checks.length, failed: result.checks.filter(c => !c.passed)}));
process.exitCode = result.passed ? 0 : 1;
