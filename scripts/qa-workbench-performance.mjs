#!/usr/bin/env node
// Real native self-driving fixture with external process CPU/RSS/footprint.
// Usage: qa-workbench-performance.mjs BINARY ABSOLUTE_OUTPUT_DIR ENTRIES [baseline|candidate]
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {spawn, execFileSync} from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const [binaryArg, outputArg, countArg, mode = 'baseline'] = process.argv.slice(2);
assert.ok(binaryArg && outputArg && countArg, 'binary, output and entry count required');
assert.ok(path.isAbsolute(outputArg), 'output must be absolute');
assert.ok(['baseline', 'candidate'].includes(mode));
const binary = fs.realpathSync(binaryArg), output = path.resolve(outputArg), count = Number(countArg);
assert.ok(Number.isSafeInteger(count) && count >= 1200);
assert.ok(!fs.existsSync(output), 'use a new evidence directory');
const rows = execFileSync('ps', ['-axo', 'pid=,comm='], {encoding:'utf8'}).split('\n');
const conflicting = rows.filter(row => /\/(?:luna|chat-workbench-fixture|workbench-performance-fixture|baseline-native-executable)$/.test(row.trim()));
assert.deepEqual(conflicting, [], 'gracefully quit other LUNA GUI processes first');
const builders = rows.filter(row => /\/(?:cargo|rustc|mbx)$/.test(row.trim()));
assert.deepEqual(builders, [], 'measure without another Cargo build');
execFileSync('codesign', ['--verify', '--strict', binary]);
const helper = path.join(root, 'target/qa/harness-evolution/slice-6/macos-resource-stat');
assert.ok(fs.existsSync(helper), 'compile scripts/macos-resource-stat.c before running');
fs.mkdirSync(output, {recursive:true});
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const manifest = {
  binary, binarySha256:hash(binary), mode, entries:count, signatureValid:true,
  revision:execFileSync('git', ['rev-parse','HEAD'], {cwd:root,encoding:'utf8'}).trim(),
  fixtureSourceSha256:hash(path.join(root,'crates/ui/examples/workbench-performance-fixture.rs')),
  allocator:'workspace mimalloc v2 on macOS', startedAt:new Date().toISOString(),
  sampleIntervalMs:200,
};
const save = (name, value) => fs.writeFileSync(path.join(output,name), `${JSON.stringify(value,null,2)}\n`);
save('process-manifest.json', manifest);
const log = fs.openSync(path.join(output,'fixture.log'),'w');
const child = spawn(binary,[output,String(count)], {
  cwd:root, env:{...process.env,LUNA_PERF_MODE:mode}, stdio:['ignore',log,log],
});
assert.ok(child.pid);manifest.pid=child.pid;save('process-manifest.json',manifest);
// A scoped display assertion prevents an idle screen from changing native
// frame cadence; caffeinate exits automatically with this fixture process.
const awake = spawn('/usr/bin/caffeinate',['-d','-w',String(child.pid)],{stdio:'ignore'});
awake.unref();
const samples = [];
let runtimeVerified = false;
function sample() {
  try {
    const stat = JSON.parse(execFileSync(helper,[String(child.pid)],{encoding:'utf8',timeout:3000}))[0];
    if (stat) {
      const value = {unixMs:Date.now(),...stat};samples.push(value);
      fs.appendFileSync(path.join(output,'process-samples.jsonl'),`${JSON.stringify(value)}\n`);
    }
    const runtimePath=path.join(output,'runtime.json');
    if (!runtimeVerified && fs.existsSync(runtimePath)) {
      const runtime=JSON.parse(fs.readFileSync(runtimePath));assert.equal(runtime.pid,child.pid);
      const processPath=execFileSync('ps',['-p',String(child.pid),'-o','comm='],{encoding:'utf8'}).trim();
      assert.equal(fs.realpathSync(processPath),binary);
      const listeners=execFileSync('lsof',['-nP',`-iTCP:${runtime.ipcPort}`,'-sTCP:LISTEN','-Fp'],{encoding:'utf8'});
      assert.deepEqual([...new Set(listeners.match(/^p\d+$/gm))],[`p${child.pid}`]);
      save('runtime-readback.json',{...runtime,processPath,listenerPid:child.pid,verified:true});runtimeVerified=true;
    }
  } catch (error) {
    // Process startup/exit can race the sampler. Retain failures; completion
    // still requires one successful identity/listener readback.
    fs.appendFileSync(path.join(output,'sample-errors.log'),`${Date.now()} ${String(error)}\n`);
  }
}
sample();
const timer=setInterval(sample,200);
const outcome=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));});
clearInterval(timer);fs.closeSync(log);
manifest.finishedAt=new Date().toISOString();manifest.outcome=outcome;
manifest.binaryUnchanged=manifest.binarySha256===hash(binary);manifest.runtimeVerified=runtimeVerified;
save('process-manifest.json',manifest);
assert.ok(manifest.binaryUnchanged && runtimeVerified,'executable and live listener must be verified');
const marksPath=path.join(output,'phase-marks.jsonl');
const marks=fs.existsSync(marksPath) ? fs.readFileSync(marksPath,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
const phases={};
for (const start of marks.filter(m=>m.edge==='start')) {
  const end=marks.find(m=>m.phase===start.phase && m.edge==='end' && m.unixMs>=start.unixMs);
  if (!end) continue;
  const before=samples.filter(s=>s.unixMs<=start.unixMs).at(-1) ?? samples[0];
  const after=samples.find(s=>s.unixMs>=end.unixMs) ?? samples.at(-1);
  const inside=samples.filter(s=>s.unixMs>=start.unixMs && s.unixMs<=end.unixMs);
  if (!before || !after) continue;
  phases[start.phase]={
    wallMs:end.unixMs-start.unixMs, sampledCpuMs:(after.cpuSeconds-before.cpuSeconds)*1000,
    sampleWindowMs:after.unixMs-before.unixMs,samples:inside.length,
    rssStartMiB:before.rssMiB,rssEndMiB:after.rssMiB,
    rssPeakMiB:Math.max(before.rssMiB,after.rssMiB,...inside.map(s=>s.rssMiB)),
    footprintStartMiB:before.footprintMiB,footprintEndMiB:after.footprintMiB,
    footprintPeakMiB:Math.max(before.footprintMiB,after.footprintMiB,...inside.map(s=>s.footprintMiB)),
  };
}
save('process-phases.json',{measurement:'whole fixture process including UI, embedded engine and streaming thread; 200 ms external sampling',phases});
const resultPath=path.join(output,'results.json');
const results=fs.existsSync(resultPath) ? JSON.parse(fs.readFileSync(resultPath)) : null;
if (outcome.code===0) assert.ok(results,'successful fixture must have results.json');
// Compare only the timed workload shared with the immutable baseline.
// Later correctness scenarios deliberately resize the native window; their
// frame observations remain available separately, never mixed into latency
// acceptance. Resize-only runs still validate their own foreground frames.
const measuredPhases=['coldOpen','reopen','panes1','panes2','panes4','childUpdates','rewrites','ownership'];
const resizeOnly=process.env.LUNA_PERF_PHASES==='resize';
const timingValidation={scope:resizeOnly?'resize-only':'matched-benchmark-phases',observations:0,inactive:0,missing:0};
const postMeasurement={observations:0,inactive:0,missing:0};
function inspectTimings(value, target) {
  if (!value || typeof value!=='object') return;
  if (Object.hasOwn(value,'windowActive')) {
    target.observations++;
    if (value.windowActive!==true) target.inactive++;
    if (typeof value.inputToPresentedMs!=='number') target.missing++;
  }
  if (Object.hasOwn(value,'windowActiveSamples')) {
    target.observations+=value.windowActiveSamples+value.inactiveWindowSamples;
    target.inactive+=value.inactiveWindowSamples;
    target.missing+=value.missingFrameSamples;
  }
  for (const child of Object.values(value)) inspectTimings(child,target);
}
if (results) {
  for (const name of resizeOnly?['resize']:measuredPhases) assert.ok(results.phases[name],`missing timed phase ${name}`);
  for (const [name,phase] of Object.entries(results.phases)) {
    const measured=resizeOnly?name==='resize':measuredPhases.includes(name);
    inspectTimings(phase,measured?timingValidation:postMeasurement);
  }
}
save('timing-validation.json',{...timingValidation,postMeasurement});
if (outcome.code===0) assert.ok(timingValidation.observations>0 && timingValidation.inactive===0 && timingValidation.missing===0,'accept timings only from completed foreground frames');
// GPUI's application run loop can return success after the fixture's task
// records an error and quits. The durable checks, not that loop status, decide
// candidate acceptance.
const failedChecks = results?.failedChecks ?? null;
const checksFailed = mode==='candidate' && (!Array.isArray(failedChecks) || failedChecks.length>0);
const effectiveCode = outcome.code!==0 ? (outcome.code ?? 1) : (checksFailed ? 1 : 0);
save('acceptance.json',{fixtureExit:outcome.code,checksFailed,effectiveCode,failedChecks});
console.log(JSON.stringify({output,...outcome,effectiveCode,runtimeVerified,phases:Object.keys(phases),failedChecks}));
process.exitCode=effectiveCode;
