#!/usr/bin/env node
// Smoke-test a packed Gemini CLI provider without LUNA and without a model:
// start its bin/worker with a fake `gemini` first on PATH (a stand-in that
// prints Gemini's stream-json frames), then speak worker protocol 1.0 and
// provider profile 1.0 to it the way LUNA's bridge does.
//
//   node scripts/smoke.mjs <package dir>
//
// Checks: the hello, describe (the fake CLI's path and version), models (the
// fake CLI's own catalog module), a completed run with its prompt on stdin,
// Stop of a hanging run (its process is gone), a refused Auto start, and a
// clean exit on shutdown. Prints a JSON summary; exits 1 on any failure.

import { spawn } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, existsSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';

const packageDir = process.argv[2] && resolve(process.argv[2]);
if (!packageDir || !existsSync(join(packageDir, 'luna-plugin.json'))) {
  console.error('usage: smoke.mjs <package dir>');
  process.exit(2);
}
const manifest = JSON.parse(readFileSync(join(packageDir, 'luna-plugin.json'), 'utf8'));

// A fake Gemini CLI install: bin/gemini -> cli/dist/index.js, with its own
// package.json and a core package exporting the model catalog.
const fake = mkdtempSync(join(tmpdir(), 'luna-gemini-smoke-'));
const cliDir = join(fake, 'cli');
const coreConfig = join(cliDir, 'node_modules', '@google', 'gemini-cli-core', 'dist', 'src', 'config');
mkdirSync(join(cliDir, 'dist'), { recursive: true });
mkdirSync(coreConfig, { recursive: true });
mkdirSync(join(fake, 'bin'));
mkdirSync(join(fake, 'work'));
writeFileSync(
  join(cliDir, 'package.json'),
  JSON.stringify({ name: '@google/gemini-cli', version: '0.0.0-smoke', type: 'module' }),
);
writeFileSync(
  join(cliDir, 'node_modules', '@google', 'gemini-cli-core', 'package.json'),
  JSON.stringify({ name: '@google/gemini-cli-core', version: '0.0.0-smoke', type: 'module' }),
);
writeFileSync(
  join(coreConfig, 'models.js'),
  [
    "export const PREVIEW_GEMINI_MODEL_AUTO = 'auto-smoke';",
    "export const DEFAULT_GEMINI_MODEL_AUTO = 'auto-smoke-old';",
    "export const VALID_GEMINI_MODELS = new Set(['smoke-pro', 'smoke-hidden']);",
    "export function getDisplayString(model) { return model === 'auto-smoke' ? 'Auto (Smoke)' : model; }",
    "export function isActiveModel(model) { return model !== 'smoke-hidden'; }",
    "export function isPreviewModel(model) { return model === 'smoke-pro'; }",
  ].join('\n'),
);
const fakeCli = `#!${process.execPath}
// Stand-in for gemini: records its argv and stdin, prints stream-json.
import { writeFileSync } from 'node:fs';
const args = process.argv.slice(2);
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { input += chunk; });
process.stdin.on('end', () => {
  writeFileSync('fake-gemini.json', JSON.stringify({ args, input, pid: process.pid }));
  const emit = (event) => process.stdout.write(JSON.stringify(event) + '\\n');
  const resumed = args.indexOf('--resume');
  emit({ type: 'init', session_id: resumed >= 0 ? args[resumed + 1] : 'smoke-session', model: 'smoke-pro' });
  emit({ type: 'message', role: 'user', content: input });
  if (input.includes('hang')) {
    emit({ type: 'message', role: 'assistant', content: 'HANGING', delta: true });
    setInterval(() => undefined, 1000);
    return;
  }
  emit({ type: 'message', role: 'assistant', content: 'SMOKE-OK', delta: true });
  emit({ type: 'tool_use', tool_name: 'run_shell_command', tool_id: 'call-1', parameters: { command: 'echo hi' } });
  emit({ type: 'tool_result', tool_id: 'call-1', status: 'success', output: 'hi' });
  emit({ type: 'result', status: 'success', stats: { input_tokens: 5, output_tokens: 2 } });
  process.exit(0);
});
`;
writeFileSync(join(cliDir, 'dist', 'index.js'), fakeCli);
chmodSync(join(cliDir, 'dist', 'index.js'), 0o755);
symlinkSync(join(cliDir, 'dist', 'index.js'), join(fake, 'bin', 'gemini'));

const worker = spawn(join(packageDir, 'bin', 'worker'), [], {
  cwd: packageDir,
  env: { PATH: [join(fake, 'bin'), process.env.PATH ?? ''].join(delimiter), HOME: process.env.HOME ?? fake },
  stdio: ['pipe', 'pipe', 'inherit'],
});
const replies = new Map();
const waiting = new Map();
createInterface({ input: worker.stdout }).on('line', (line) => {
  const frame = JSON.parse(line);
  const done = waiting.get(frame.id);
  if (done) {
    waiting.delete(frame.id);
    done(frame);
  } else {
    replies.set(frame.id, frame);
  }
});
let next = 0;
const send = (frame) => worker.stdin.write(`${JSON.stringify(frame)}\n`);
const reply = (id, ms = 15000) =>
  new Promise((resolveReply, reject) => {
    if (replies.has(id)) {
      resolveReply(replies.get(id));
      return;
    }
    const timer = setTimeout(() => reject(new Error(`no reply to ${id} within ${ms} ms`)), ms);
    waiting.set(id, (frame) => {
      clearTimeout(timer);
      resolveReply(frame);
    });
  });
const call = async (method, params = {}, deadlineMs = 10000) => {
  const id = `call-${(next += 1)}`;
  send({ type: 'call', id, method, params, deadlineMs });
  return reply(id, deadlineMs + 2000);
};
// One cursor per run, kept across reads like LUNA's bridge keeps it: `after`
// acknowledges every event at or below it, and the worker frees those, so a
// reader never starts a run over from 0.
const cursors = new Map();
const events = async (runId, until) => {
  const seen = [];
  let after = cursors.get(runId) ?? 0;
  for (let round = 0; round < 50; round += 1) {
    const page = await call('provider.run.events', { runId, after, waitMs: 2000 }, 5000);
    if (!page.ok) throw new Error(`events failed: ${JSON.stringify(page.error)}`);
    for (const event of page.result.events) {
      if (event.seq <= after) continue;
      if (event.seq !== after + 1) throw new Error(`event ${event.seq} after ${after}`);
      after = event.seq;
      cursors.set(runId, after);
      seen.push(event);
    }
    if (page.result.done || until(seen)) return seen;
  }
  throw new Error('the run never finished');
};
const check = (condition, what) => {
  if (!condition) throw new Error(what);
};
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

const summary = { packageDir, fakeInstall: fake };
try {
  send({
    type: 'hello',
    id: 'hello-1',
    protocol: { major: 1, minor: 0 },
    api: { major: 1, minor: 1 },
    instance: { id: 'smoke', extensionId: manifest.id, version: manifest.version, packageHash: 'smoke', trust: 'native', grants: [], target: { kind: 'none' } },
    limits: {},
    required: [],
  });
  const hello = await reply('hello-1', 3000);
  check(hello.type === 'hello' && hello.protocol.major === 1, `bad hello ${JSON.stringify(hello)}`);
  check(hello.operations.includes('provider.run.stop'), 'the hello lacks provider.run.stop');
  summary.operations = hello.operations;

  const described = await call('provider.describe');
  check(described.ok && described.result.available, `describe ${JSON.stringify(described)}`);
  check(described.result.providerId === manifest.id, 'describe names another provider');
  check(described.result.executable.version === '0.0.0-smoke', 'describe did not read the CLI version');
  summary.describe = described.result;

  const listed = await call('provider.models');
  const ids = listed.result.models.map((model) => model.id);
  check(JSON.stringify(ids) === JSON.stringify(['auto-smoke', 'auto-smoke-old', 'smoke-pro']), `models ${ids}`);
  summary.models = listed.result.models;

  const cwd = join(fake, 'work');
  const started = await call('provider.run.start', {
    prompt: 'say ok',
    cwd,
    model: 'smoke-pro',
    modelOptions: {},
    permissionMode: 'full',
    resume: 'session-7',
    attachments: [],
  });
  check(started.ok, `run.start ${JSON.stringify(started)}`);
  const completed = await events(started.result.runId, () => false);
  const kinds = completed.map((event) => event.type);
  check(kinds[0] === 'session' && completed[0].sessionId === 'session-7', 'the resumed session was not reported first');
  check(completed.some((event) => event.type === 'text' && event.text === 'SMOKE-OK'), 'no assistant text');
  check(completed.some((event) => event.type === 'toolCall' && event.call?.kind === 'exec'), 'no exec tool call');
  check(kinds.at(-1) === 'done' && completed.at(-1).status === 'completed', `ended ${JSON.stringify(completed.at(-1))}`);
  const recorded = JSON.parse(readFileSync(join(cwd, 'fake-gemini.json'), 'utf8'));
  check(recorded.input === 'say ok', 'the prompt did not arrive on stdin');
  check(!recorded.args.includes('say ok'), 'the prompt appeared in argv');
  check(recorded.args.join(' ').includes('--approval-mode yolo'), 'Full access did not map to yolo');
  summary.completed = { events: kinds, args: recorded.args };

  const hanging = await call('provider.run.start', {
    prompt: 'hang please',
    cwd,
    modelOptions: {},
    permissionMode: 'full',
    attachments: [],
  });
  check(hanging.ok, `hanging run.start ${JSON.stringify(hanging)}`);
  await events(hanging.result.runId, (seen) => seen.some((event) => event.type === 'text'));
  const hangingPid = JSON.parse(readFileSync(join(cwd, 'fake-gemini.json'), 'utf8')).pid;
  check(alive(hangingPid), 'the hanging CLI is not running');
  const stopped = await call('provider.run.stop', { runId: hanging.result.runId });
  check(stopped.ok && stopped.result.stopping, `stop ${JSON.stringify(stopped)}`);
  const ending = await events(hanging.result.runId, () => false);
  check(ending.at(-1).type === 'done' && ending.at(-1).status === 'interrupted', `stop ended ${JSON.stringify(ending.at(-1))}`);
  check(!alive(hangingPid), 'Stop left the CLI running');
  summary.stopped = { pid: hangingPid, ending: ending.map((event) => event.type) };

  const auto = await call('provider.run.start', {
    prompt: 'auto',
    cwd,
    modelOptions: {},
    permissionMode: 'auto',
    attachments: [],
  });
  check(!auto.ok && auto.error.code === 'rejected', `Auto was not refused: ${JSON.stringify(auto)}`);
  summary.autoRefused = auto.error.message;

  send({ type: 'shutdown' });
  worker.stdin.end();
  const code = await new Promise((resolveExit) => worker.once('exit', resolveExit));
  check(code === 0, `the worker exited with ${code}`);
  summary.ok = true;
  console.log(JSON.stringify(summary, null, 2));
} catch (error) {
  summary.ok = false;
  summary.error = error instanceof Error ? error.message : String(error);
  console.log(JSON.stringify(summary, null, 2));
  worker.kill('SIGKILL');
  process.exit(1);
}
