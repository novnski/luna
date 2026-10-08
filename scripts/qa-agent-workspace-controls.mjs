#!/usr/bin/env node
// Real-provider acceptance in this feature's isolated signed desktop profile.
// Never targets the primary app or an existing user project.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = fs.realpathSync(path.resolve(import.meta.dirname, '..'));
const output = path.resolve(process.env.AGENT_WORKSPACE_NATIVE_DIR || path.join(root, 'target/qa/agent-workspace-controls/native'));
assert.ok(output.startsWith(path.join(root, 'target') + path.sep), 'Native evidence must stay under this checkout target');
const identityKeys = ['LUNA_DEV_APP_NAME', 'LUNA_DEV_BUNDLE_ID', 'LUNA_DEV_IPC_PORT', 'LUNA_DEV_DATA_DIR'];
const suppliedIdentity = identityKeys.filter(key => Object.hasOwn(process.env, key));
assert.ok(suppliedIdentity.length === 0 || suppliedIdentity.length === 4, 'QA identity overrides require all four LUNA_DEV_* values');
const overridden = suppliedIdentity.length === 4;
for (const key of suppliedIdentity) assert.match(process.env[key], /^\S(?:[^\r\n]*\S)?$/, `${key} must be nonempty and single-line`);
const appName = overridden ? process.env.LUNA_DEV_APP_NAME : 'LUNA QA Agent Workspace';
const bundleId = overridden ? process.env.LUNA_DEV_BUNDLE_ID : 'com.novnski.luna.qa.agent-workspace';
const portValue = overridden ? process.env.LUNA_DEV_IPC_PORT : '59804';
assert.match(appName, /^LUNA QA [A-Za-z0-9][A-Za-z0-9 -]{0,48}$/);
assert.match(bundleId, /^com\.novnski\.luna\.qa\.[a-z][a-z0-9-]{0,47}$/);
assert.match(portValue, /^[1-9][0-9]{0,4}$/);
const port = Number(portValue);
assert.ok(port >= 1024 && port <= 65535 && port !== 59777, 'A separate QA IPC port is required');
const dataDir = overridden ? process.env.LUNA_DEV_DATA_DIR : path.join(output, 'data');
assert.ok(path.isAbsolute(dataDir));
const bundle = path.join(root, 'target/luna-macos-dev', `${appName}.app`);
const target = path.join(root, 'target');
assert.equal(fs.realpathSync(target), target, 'QA target must not redirect through symlinks');
assert.equal(fs.realpathSync(dataDir), dataDir, 'QA data path must be canonical');
assert.ok(dataDir.startsWith(`${target}${path.sep}`), 'QA data must be inside this checkout target');
const staging = path.join(target, 'luna-macos-dev');
assert.ok(dataDir !== staging && !dataDir.startsWith(`${staging}${path.sep}`), 'QA data must be outside bundle staging and primary data');
assert.equal(fs.realpathSync(bundle), bundle, 'QA bundle must not redirect through symlinks');
const mode = process.argv[2] || 'snapshot';
assert.ok(['seed', 'start', 'resume', 'snapshot', 'verify'].includes(mode));
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], { encoding: 'utf8' }));
assert.equal(plist.CFBundleIdentifier, bundleId);
assert.equal(plist.CFBundleDisplayName, appName);
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT, String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, dataDir);
assert.equal(plist.LSEnvironment.LUNA_QA_MCP_ISOLATION, '1', 'QA requires signed MCP isolation');
const pids = execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], { encoding: 'utf8' }).match(/^p\d+$/gm) ?? [];
assert.equal(new Set(pids).size, 1, 'Exactly one isolated QA app must be listening');
const pid = pids[0].slice(1);
assert.equal(execFileSync('ps', ['-p', pid, '-o', 'comm='], { encoding: 'utf8' }).trim(), executable);
fs.mkdirSync(output, { recursive: true });

const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
const call = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++seq;
  const timeout = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timeout`)); }, 45000);
  function receive(event) {
    for (const line of String(event.data).split('\n').filter(Boolean)) {
      const frame = JSON.parse(line);
      if (frame.id !== id || !['item', 'ok', 'err'].some(key => Object.hasOwn(frame, key))) continue;
      clearTimeout(timeout); ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(`${method}: ${JSON.stringify(frame.err)}`));
      else {
        if (Object.hasOwn(frame, 'item')) ws.send(JSON.stringify({ id, cancel: true }));
        resolve(Object.hasOwn(frame, 'item') ? frame.item : frame.ok);
      }
    }
  }
  ws.addEventListener('message', receive); ws.send(JSON.stringify({ id, method, params }));
});
const save = (name, value) => fs.writeFileSync(path.join(output, `${name}.json`), JSON.stringify(value, null, 2) + '\n');
save('app-identity', { checkedAt: new Date().toISOString(), pid: Number(pid), executable, port,
  bundleId: plist.CFBundleIdentifier, dataDir: plist.LSEnvironment.LUNA_DATA_DIR,
  appName, mcpIsolation: plist.LSEnvironment.LUNA_QA_MCP_ISOLATION,
  signatureVerified: true, binarySha256: crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex') });
const manifestPath = path.join(output, 'manifest.json');
const entries = value => value.frame?.reset ?? value.reset ?? [];
const text = entry => (entry.parts || []).filter(part => part.kind === 'text').map(part => part.text).join('\n');
const tabs = node => node.kind === 'pane' ? node.tabs : [...tabs(node.first), ...tabs(node.second)];
const rows = value => Array.isArray(value) ? value : value.chats ?? value.sessions ?? value.items ?? [];
const selection = model => ({
  model: model.id,
  reasoning: model.reasoningLevels.includes('high') ? 'high' : model.reasoningLevels.includes('medium') ? 'medium' : model.reasoningLevels[0] ?? null,
  modelOptions: Object.fromEntries(model.options.map(option => [option.id, option.defaultChoice])),
});
let manifest;
try {
  if (mode === 'seed') {
    assert.ok(!fs.existsSync(manifestPath), 'Already seeded; reuse the existing manifest');
    const info = await call('EngineInfo');
    const catalogs = {};
    for (const provider of ['codex', 'claude-code']) {
      catalogs[provider] = await call('ListModels', { harness: provider });
      save(`catalog-${provider}`, catalogs[provider]);
    }
    const codexParent = catalogs.codex.find(model => model.id === 'gpt-6.1-sol') ?? catalogs.codex[0];
    const codexChild = catalogs.codex.find(model => model.id === 'gpt-6-luna') ?? codexParent;
    const claude = catalogs['claude-code'].find(model => /sonnet/i.test(model.id)) ?? catalogs['claude-code'][0];
    assert.ok(codexParent && codexChild && claude, 'Both native providers need a catalog');
    const defaults = { codex: selection(codexChild), 'claude-code': selection(claude) };
    let saved = await call('GetProviderDefaults');
    for (const provider of ['codex', 'claude-code']) {
      saved = await call('SetProviderDefaults', { expectedRevision: saved.revision, provider, defaults: defaults[provider] });
    }
    const project = path.join(output, 'project');
    fs.mkdirSync(project, { recursive: true });
    fs.writeFileSync(path.join(project, 'README.md'), 'Isolated LUNA agent workspace QA. Do not edit files or invoke external tools.\n');
    const space = crypto.randomUUID();
    await call('Mutate', { op: 'createSpace', spaceId: space, deviceId: info.deviceId, path: project });
    manifest = { project, space, deviceId: info.deviceId, profileId: info.profileId, defaults, parents: {}, binarySha256: crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex') };
    for (const [provider, permissionMode, model] of [['codex', 'full', codexParent], ['claude-code', 'auto', claude]]) {
      const id = crypto.randomUUID();
      manifest.parents[provider] = id;
      await call('Mutate', { op: 'createChat', chatId: id, spaceId: space, cwd: project, config: {
        harness: provider, ...selection(model), sandbox: permissionMode === 'full' ? 'danger-full-access' : 'workspace-write', permissionMode, effectivePermissionMode: permissionMode,
      } });
      await call('Mutate', { op: 'renameChat', chatId: id, title: `Workspace QA · ${provider} · ${permissionMode}` });
    }
    save('manifest', manifest); save('defaults-saved', saved);
    console.log(JSON.stringify({ phase: 'seeded', parents: manifest.parents, defaults }));
  } else {
    manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
    if (mode === 'start') {
      for (const [provider, parent] of Object.entries(manifest.parents)) {
        const target = provider === 'codex' ? 'claude-code' : 'codex';
        const request = { prompt: 'Reply exactly WORKSPACE_HELPER_OK. Do not use tools or read or change files.', idempotencyKey: `workspace-native-${provider}`, title: `Workspace helper · ${target}`, provider: target, reportResults: true, continueParent: false };
        const prompt = `This is an authorized LUNA app E2E test in a disposable project. Use only the attached LUNA tools. First call get_workspace. Then call create_thread exactly once with ${JSON.stringify(request)}. Omit model, reasoning, modelOptions and permissionMode so the saved provider defaults and parent access ceiling apply. Do not use shell, filesystem or external tools. After creation, call get_workspace again and confirm its background tab left the main pane selected. Wait for the child with wait_threads and read_thread, then reply exactly WORKSPACE_FRESH_OK. Do not start further children or queue a continuation. Reuse this exact idempotency key only if a response is lost.`;
        await call('QueueMessage', { chatId: parent, messageId: `workspace-fresh-${provider}`, text: prompt, holdForTurnEnd: true });
        save(`request-fresh-${provider}`, { parent, request, prompt });
      }
      console.log(JSON.stringify({ phase: 'started', parents: manifest.parents }));
    }
    if (mode === 'resume') {
      for (const [provider, parent] of Object.entries(manifest.parents)) {
        const permissionMode = provider === 'codex' ? 'auto' : 'full';
        await call('Mutate', { op: 'setThreadPermissionMode', chatId: parent, mode: permissionMode });
        const prompt = `Continue this authorized LUNA E2E test using only LUNA tools. Read get_workspace and list_threads. Show the existing helper in a background split on the right using manage_workspace openTab with an exact current layout revision, the focused pane node ID, and new UUID pane/split IDs. Confirm Main kept focus, then close only the helper's tab using closeTabs and read_thread to confirm its conversation remains. Next fork THIS calling conversation using manage_workspace forkThread, nativeContext:false, idempotencyKey:workspace-native-fork-${provider}, and the current layout revision. This must be a frozen transcript snapshot and an idle persistent fork. Send the new fork this exact message once with send_message_to_thread: "Reply exactly WORKSPACE_FORK_OK. Do not use tools or read or change files." Use idempotencyKey workspace-native-fork-send-${provider}. Wait/read its result. Finally archive the original helper and the new fork using one manage_workspace archiveThreads request with those two exact IDs, closeTabs:true, the current layout revision, and idempotencyKey workspace-native-archive-${provider}. Never archive this caller or any unrelated chat. Reply exactly WORKSPACE_RESUME_OK when all operations succeed. Do not call shell, filesystem, external tools, or create_thread.`;
        await call('QueueMessage', { chatId: parent, messageId: `workspace-resume-${provider}`, text: prompt, holdForTurnEnd: true });
        save(`request-resume-${provider}`, { parent, permissionMode, prompt });
      }
      console.log(JSON.stringify({ phase: 'resumed', parents: manifest.parents }));
    }
    const chats = rows(await call('WatchChats'));
    const sessions = rows(await call('WatchSessions'));
    const result = {};
    for (const [provider, parentId] of Object.entries(manifest.parents)) {
      const parent = chats.find(chat => chat.id === parentId);
      const children = chats.filter(chat => chat.childThread?.parentTaskId === parentId);
      const parentMessages = entries(await call('WatchDocMessages', { chatId: parentId }));
      const layout = await call('GetChatWorkbench', { mainChatId: parentId });
      const forks = await call('ListSideChats', { parentId });
      const forkSnapshots = [];
      for (const fork of forks.items) forkSnapshots.push({
        summary: fork,
        chat: chats.find(chat => chat.id === fork.id),
        messages: entries(await call('WatchDocMessages', { chatId: fork.id })),
      });
      const childSnapshots = [];
      for (const child of children) childSnapshots.push({ chat: child, messages: entries(await call('WatchDocMessages', { chatId: child.id })), session: sessions.find(session => session.chatId === child.id) });
      const approvals = parentMessages.flatMap(entry => entry.parts ?? []).filter(part => part.kind === 'input' && part.questions?.some(question => question.approval));
      result[provider] = { parent, parentMessages, children: childSnapshots, forks, forkSnapshots, layout, session: sessions.find(session => session.chatId === parentId), approvalCount: approvals.length };
      if (mode === 'verify') {
        assert.equal(approvals.length, 0, `${provider} unexpectedly showed an approval`);
        assert.equal(children.length, 1, `${provider} created duplicate helpers`);
        const helper = children[0];
        const target = provider === 'codex' ? 'claude-code' : 'codex';
        assert.equal(helper.config.harness, target);
        assert.equal(helper.config.model, manifest.defaults[target].model);
        assert.equal(helper.config.reasoning, manifest.defaults[target].reasoning);
        for (const [key, choice] of Object.entries(manifest.defaults[target].modelOptions)) assert.equal(helper.config.modelOptions[key], choice);
        const helperUsers = childSnapshots[0].messages.filter(entry => entry.role === 'user');
        assert.equal(helperUsers.length, 1, 'A fresh helper receives only its brief');
        assert.equal(text(helperUsers[0]), helper.childThread.initialPrompt);
        assert.ok(childSnapshots[0].messages.some(entry => entry.role === 'assistant' && text(entry).includes('WORKSPACE_HELPER_OK')));
        assert.ok(parentMessages.some(entry => entry.role === 'assistant' && text(entry).includes('WORKSPACE_FRESH_OK')), `${provider} fresh run not complete`);
        assert.equal(parent.archived, false);
        const expectResume = fs.existsSync(path.join(output, `request-resume-${provider}.json`));
        assert.ok(!result[provider].session || result[provider].session.status === 'idle', `${provider} is not idle`);
        const resumed = parentMessages.some(entry => entry.role === 'assistant' && text(entry).includes('WORKSPACE_RESUME_OK'));
        if (expectResume) assert.ok(resumed, `${provider} resumed workflow is not complete`);
        if (resumed) {
          assert.equal(helper.archived, true);
          assert.equal(forks.items.length, 1, `${provider} must have exactly one persistent fork`);
          const fork = chats.find(chat => chat.id === forks.items[0].id);
          assert.ok(fork && fork.archived, 'The fork must be archived too');
          assert.equal(forks.items[0].contextMode, 'contextSnapshot');
          assert.ok(forkSnapshots[0].messages.some(entry => entry.role === 'assistant' && text(entry).includes('WORKSPACE_FORK_OK')), 'The actual fork must reply');
          assert.ok(!tabs(layout.root).some(tab => tab.chatId === fork.id));
          assert.ok(!tabs(layout.root).some(tab => tab.chatId === helper.id));
        } else {
          assert.ok(tabs(layout.root).some(tab => tab.chatId === helper.id), 'Helper background tab is missing');
          assert.equal(layout.root.activeChatId, parentId);
        }
      }
    }
    if (mode === 'verify') {
      const defaults = await call('GetProviderDefaults');
      for (const [provider, expected] of Object.entries(manifest.defaults)) assert.deepEqual(defaults.providers[provider], expected);
      save('defaults-readback', defaults);
    }
    save(`snapshot-${mode}-${Date.now()}`, result);
    console.log(JSON.stringify(Object.fromEntries(Object.entries(result).map(([provider, value]) => [provider, {
      parent: value.parent?.id, status: value.session?.status, approvalCount: value.approvalCount,
      children: value.children.map(child => ({ id: child.chat.id, model: child.chat.config.model, archived: child.chat.archived, status: child.session?.status })),
      freshComplete: value.parentMessages.some(entry => entry.role === 'assistant' && text(entry).includes('WORKSPACE_FRESH_OK')),
      resumeComplete: value.parentMessages.some(entry => entry.role === 'assistant' && text(entry).includes('WORKSPACE_RESUME_OK')),
    }]))));
  }
} finally { ws.close(); }
