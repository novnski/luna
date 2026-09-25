#!/usr/bin/env node
// Read-only end-to-end check against a running LUNA engine. No transcript or
// account data is printed; the same local RPC and periods are used by Usage.
const port = Number(process.env.LUNA_IPC_PORT || 59777);
const daysToRead = process.argv.slice(2).length ? process.argv.slice(2).map(Number) : [1, 7, 30, 90];
if (daysToRead.some((days) => ![1, 7, 30, 90].includes(days))) {
  throw new Error('Choose 1, 7, 30, or 90 days');
}
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { ws.removeEventListener('message', onMessage); reject(new Error(`${method} timed out`)); }, 125000);
    function onMessage(event) {
      const frame = JSON.parse(event.data);
      if (frame.id !== id) return;
      clearTimeout(timeout);
      ws.removeEventListener('message', onMessage);
      if (frame.err) reject(new Error(frame.err));
      else resolve(frame.ok);
    }
    ws.addEventListener('message', onMessage);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
function windowFor(days, now = new Date()) {
  if (days === 1) {
    const untilMs = Math.floor(now.getTime() / 60000) * 60000;
    const first = untilMs - 24 * 3600000;
    return { periodStartsMs: Array.from({ length: 24 }, (_, i) => first + i * 3600000), untilMs };
  }
  const periodStartsMs = Array.from({ length: days }, (_, index) =>
    new Date(now.getFullYear(), now.getMonth(), now.getDate() - (days - index - 1)).getTime());
  const untilMs = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1).getTime();
  return { periodStartsMs, untilMs };
}
try {
  for (const days of daysToRead) {
    const params = { days, ...windowFor(days), includeLimits: process.env.LUNA_USAGE_QA_LIMITS === '1', priceOverrides: {} };
    const start = performance.now();
    const snapshot = await call('GetUsageSnapshot', params);
    const elapsedMs = Math.round(performance.now() - start);
    const { summary } = snapshot;
    const providers = summary.sources.filter((source) => source.status === 'ready' || source.status === 'partial');
    const models = [...new Set(summary.buckets.map((bucket) => bucket.model))].sort();
    const periodsWithData = new Set(summary.buckets.map((bucket) => bucket.startMs)).size;
    const allPeriodStartsMatch = summary.buckets.every((bucket) => params.periodStartsMs.includes(bucket.startMs));
    const totalTokens = summary.buckets.reduce((sum, bucket) => sum + Object.entries(bucket.tokens)
      .filter(([key]) => key !== 'reasoning').reduce((count, [, value]) => count + value, 0), 0);
    console.log(JSON.stringify({ days, elapsedMs, scanDurationMs: summary.scanDurationMs,
      partial: summary.partial, filesScanned: providers.reduce((sum, source) => sum + source.filesScanned, 0),
      filesCached: providers.reduce((sum, source) => sum + source.filesCached, 0),
      sessions: providers.reduce((sum, source) => sum + source.distinctSessions, 0),
      buckets: summary.buckets.length, periodsWithData, allPeriodStartsMatch, limits: snapshot.limits.length,
      modelCount: models.length, models, totalTokens,
      unpricedRecords: summary.buckets.reduce((sum, bucket) => sum + (bucket.unpricedRecords ?? 0), 0) }));
  }
} finally {
  ws.close();
}
