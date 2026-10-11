#!/usr/bin/env node
// CORE-00: real child-process handshake feasibility, not the production SDK.
// Pre-code cases and ownership: docs/pending-tasks/extensible-core/core-00/failures.md.
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import assert from 'node:assert/strict';
import { once } from 'node:events';

const self = fileURLToPath(import.meta.url);
const limit = 64 * 1024;
const deadline = 1000;
if (process.argv[2] === '--worker') {
  const fault = process.argv[3];
  let input = '';
  process.stdin.on('data', chunk => {
    input += chunk;
    if (!input.includes('\n')) return;
    const request = JSON.parse(input);
    const reply = { id: request.id, protocol: { major: 1, minor: 0 }, capabilities: ['provider.catalog'], kind: 'hello' };
    if (fault === 'timeout') { setInterval(() => {}, 1000); return; }
    if (fault === 'eof') { process.exit(0); }
    if (fault === 'version') reply.protocol.major = 2;
    if (fault === 'capability') reply.capabilities = [];
    if (fault === 'correlation') reply.id = 'different-request';
    if (fault === 'stderr') {
      // Withhold the handshake until the pipe drains: a host that never reads
      // stderr must time out rather than accidentally pass on an early reply.
      process.stderr.write('e'.repeat(limit * 8), () => process.stdout.write(`${JSON.stringify(reply)}\n`));
      return;
    }
    process.stdout.write(fault === 'oversize' ? 'x'.repeat(limit + 1)
      : fault === 'json' ? '{invalid}\n' : `${JSON.stringify(reply)}\n`);
  });
} else {
  const output = resolve(process.argv[2] ?? '');
  assert.ok(process.argv[2], 'usage: qa-core-worker.mjs OUTPUT_DIR');
  mkdirSync(output, { recursive: false });
  async function handshake(fault) {
    const child = spawn(process.execPath, [self, '--worker', fault], { stdio: ['pipe', 'pipe', 'pipe'], env: {} });
    const closed = once(child, 'close');
    let bytes = 0, stderrBytes = 0, stderr = Buffer.alloc(0), buffer = Buffer.alloc(0), raw;
    const request = { id: randomUUID(), kind: 'hello', protocol: { major: 1, minor: 1 }, required: ['provider.catalog'] };
    const start = performance.now();
    let observed;
    try {
      raw = await new Promise((accept, reject) => {
        let settled = false;
        const timer = setTimeout(() => finish('timeout'), deadline);
        function finish(error, result) {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          error ? reject(Error(error)) : accept(result);
        }
        child.on('error', () => finish('spawn'));
        child.stdin.on('error', () => finish('stdin'));
        child.on('exit', () => finish('eof'));
        child.stderr.on('data', chunk => {
          stderrBytes += chunk.length;
          // Always drain; retain at most one bounded log window.
          if (stderr.length < limit) stderr = Buffer.concat([stderr, chunk.subarray(0, limit - stderr.length)]);
        });
        child.stdout.on('data', chunk => {
          if (settled) return;
          bytes += chunk.length;
          if (bytes > limit) { finish('frame-too-large'); return; }
          buffer = Buffer.concat([buffer, chunk]);
          const newline = buffer.indexOf(10);
          if (newline === -1) return;
          try { finish(null, JSON.parse(buffer.subarray(0, newline).toString())); }
          catch { finish('invalid-json'); }
        });
        child.stdin.end(`${JSON.stringify(request)}\n`);
      });
      if (raw.id !== request.id) throw Error('correlation');
      if (raw.kind !== 'hello' || raw.protocol?.major !== 1 || !Number.isSafeInteger(raw.protocol?.minor) || raw.protocol.minor < 0) throw Error('protocol-version');
      if (!Array.isArray(raw.capabilities) || !request.required.every(cap => raw.capabilities.includes(cap))) throw Error('capability');
      observed = 'accepted';
    } catch (error) { observed = error.message; }
    finally {
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
      const escalation = setTimeout(() => child.kill('SIGKILL'), 250);
      await closed;
      clearTimeout(escalation);
    }
    return { fault, observed, raw, request, elapsed_ms: performance.now() - start, bytes,
      stderrBytes, storedStderr: stderr.length, exitCode: child.exitCode, signal: child.signalCode,
      reaped: child.exitCode !== null || child.signalCode !== null };
  }
  const cases = [
    ['ok', 'accepted'], ['version', 'protocol-version'], ['capability', 'capability'],
    ['json', 'invalid-json'], ['oversize', 'frame-too-large'], ['correlation', 'correlation'],
    ['eof', 'eof'], ['timeout', 'timeout'], ['stderr', 'accepted'],
  ];
  const receipts = [];
  try {
    for (const [fault, expected] of cases) {
      const receipt = await handshake(fault);
      receipts.push({ ...receipt, expected });
      assert.equal(receipt.observed, expected);
      assert.equal(receipt.reaped, true);
      assert.ok(receipt.elapsed_ms < deadline + 500);
      assert.ok(receipt.storedStderr <= limit);
      if (fault === 'stderr') {
        assert.equal(receipt.stderrBytes, limit * 8);
        assert.equal(receipt.storedStderr, limit);
      }
    }
    // Recovery and enough independent launches to retain variance.
    for (let i = 0; i < 30; i++) {
      const receipt = await handshake('ok');
      receipts.push(receipt);
      assert.equal(receipt.observed, 'accepted');
      assert.equal(receipt.reaped, true);
    }
    writeFileSync(`${output}/result.json`, JSON.stringify({ passed: true, node: process.version, frameLimit: limit, deadline, receipts }, null, 2));
    console.log(JSON.stringify({ passed: true, cases: receipts.length, output }));
  } catch (error) {
    writeFileSync(`${output}/result.json`, JSON.stringify({ passed: false, error: error.message, receipts }, null, 2));
    throw error;
  }
}
