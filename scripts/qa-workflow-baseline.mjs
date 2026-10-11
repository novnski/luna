#!/usr/bin/env node
// SW-00 read-only baseline of a running LUNA engine: execution selections,
// acknowledged runtime contracts and provider-reported integration inventory.
// Sends no model/user message, creates no chat, changes no configuration and
// toggles no MCP server. Inventory probes start the providers' own
// control-only/catalog processes exactly as Settings > Check provider does.
// A capture, not an acceptance test: `captured` only means the reads succeeded.
//
// LUNA_BASELINE_EVIDENCE_DIR=<preserved dir> [LUNA_IPC_PORT=59777] \
//   node scripts/qa-workflow-baseline.mjs
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { homedir } from 'node:os';
import assert from 'node:assert/strict';

const directory = process.env.LUNA_BASELINE_EVIDENCE_DIR;
assert.ok(directory, 'Set LUNA_BASELINE_EVIDENCE_DIR to a preserved directory outside the checkout');
const port = Number(process.env.LUNA_IPC_PORT || 59777);
const providers = ['codex', 'claude-code', 'opencode'];
const home = homedir();
const tilde = (path) => (typeof path === 'string' && path.startsWith(home) ? `~${path.slice(home.length)}` : path);

const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
let nextId = 1;
function call(method, params, timeoutMs = 120000) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timed out`)); }, timeoutMs);
    function receive(event) {
      for (const line of String(event.data).split('\n').filter(Boolean)) {
        const frame = JSON.parse(line);
        if (frame.id !== id || !['item', 'ok', 'err'].some((key) => Object.hasOwn(frame, key))) continue;
        clearTimeout(timer); ws.removeEventListener('message', receive);
        if (frame.err) reject(new Error(JSON.stringify(frame.err)));
        else {
          // A watch's first item is its snapshot; release the subscription.
          if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
          resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
        }
      }
    }
    ws.addEventListener('message', receive);
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function attempt(method, params) {
  const startedAtMs = Date.now();
  try {
    return { ok: await call(method, params), ms: Date.now() - startedAtMs };
  } catch (error) {
    return { error: String(error.message || error), ms: Date.now() - startedAtMs };
  }
}

const evidence = { evidenceKind: 'sw00-live-engine-baseline', port, capturedAt: new Date().toISOString(), captured: false };
try {
  const info = await call('EngineInfo');
  evidence.engine = { deviceId: info.deviceId, workspaceScope: info.workspaceScope, capabilities: info.capabilities };
  const spaces = await call('WatchSpaces');
  evidence.spaces = spaces.map((space) => ({ id: space.id, path: tilde(space.path), gitDetected: space.gitDetected, executionDefaults: space.executionDefaults }));
  const chats = await call('WatchChats');
  const limitations = {};
  evidence.chats = [];
  for (const chat of chats) {
    // Titles, previews and prompts stay out of the record.
    const row = {
      id: chat.id, spaceId: chat.spaceId, archived: chat.archived, cwd: tilde(chat.cwd),
      child: chat.childThread ? { createdBy: chat.childThread.provisioning?.createdBy ?? null, permissionCeiling: chat.childThread.permissionCeiling } : null,
      harness: chat.config?.harness, providerAccountId: chat.config?.providerAccountId ?? null, model: chat.config?.model ?? null,
      permissionMode: chat.config?.permissionMode ?? null, effectivePermissionMode: chat.config?.effectivePermissionMode ?? null,
      sandbox: chat.config?.sandbox ?? null, hasProviderSession: Boolean(chat.harnessSessionId), lastMessageAt: chat.lastMessageAt ?? null,
    };
    const contract = await attempt('GetRuntimeContract', { taskId: chat.id });
    if (contract.ok) {
      const view = contract.ok;
      const permissions = view.execution?.capabilities?.permissions;
      if (view.provider && permissions && !limitations[view.provider]) limitations[view.provider] = permissions;
      row.runtime = {
        providerDelivery: view.providerDelivery, unavailableReason: view.unavailableReason ?? null,
        transition: view.execution?.transition?.kind ?? null, steer: view.execution?.steer?.kind ?? null,
        nativeFork: view.execution?.capabilities?.nativeFork ?? null,
        effective: view.effective && {
          delivery: view.effective.delivery, contractVersion: view.effective.contractVersion, contractHash: view.effective.contractHash,
          acceptedAtMs: view.effective.acceptedAtMs ?? null, restartRequired: view.effective.restartRequired,
          attachedTools: view.effective.attachedTools.map((tool) => tool.name), omittedTools: view.effective.omittedTools,
          instructions: view.effective.instructions.map(({ name, source, characterCount }) => ({ name, source, characterCount })),
        },
      };
    } else row.runtime = { error: contract.error };
    evidence.chats.push(row);
  }
  evidence.providerPermissionFacts = limitations;
  evidence.inventory = [];
  for (const space of spaces) {
    for (const provider of providers) {
      const recent = chats.filter((chat) => chat.spaceId === space.id && chat.config?.harness === provider && chat.harnessSessionId)
        .sort((a, b) => String(b.lastMessageAt ?? '').localeCompare(String(a.lastMessageAt ?? '')))[0];
      const params = { provider, projectId: space.id, probeProvider: true, ...(recent ? { taskId: recent.id } : {}) };
      const result = await attempt('GetIntegrationInventory', params);
      evidence.inventory.push({
        spaceId: space.id, provider, sessionTaskId: recent?.id ?? null, ms: result.ms, error: result.error ?? null,
        unavailableReason: result.ok?.unavailableReason ?? null,
        items: (result.ok?.items ?? []).map((item) => ({
          name: item.name, kind: item.kind, source: item.source, scope: item.scope, configured: item.configured,
          available: item.available, attached: item.attached, reason: item.reason, owner: item.owner,
          mcp: item.mcp && { status: item.mcp.status, transport: item.mcp.transport, toolCount: item.mcp.toolCount, error: item.mcp.error, launches: item.mcp.launches, session: item.mcp.session, appSupport: item.mcp.appSupport },
        })),
      });
    }
  }
  assert.ok(evidence.chats.length > 0, 'The engine reported no chats');
  assert.ok(evidence.inventory.some((row) => !row.error), 'Every inventory read failed');
  evidence.captured = true;
} finally {
  ws.close();
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, 'luna-live-baseline.json'), `${JSON.stringify(evidence, null, 2)}\n`);
}
console.log(JSON.stringify({ captured: evidence.captured, chats: evidence.chats?.length ?? 0, inventoryReads: evidence.inventory?.length ?? 0, directory }));
