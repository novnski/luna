#!/usr/bin/env node
// Repeatable, real-provider acceptance. Requires the isolated signed QA app.
// Modes do not retry writes; an interrupted run is recovered from its manifest.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = fs.realpathSync(path.resolve(import.meta.dirname, '..'));
const output = path.join(root, 'target/qa/agent-delegation-ux/native');
const bundle = path.join(root, 'target/luna-macos-dev/LUNA QA Delegation UX.app');
const port = 59805;
const mode = process.argv[2] || 'snapshot';
assert.ok(['seed', 'start', 'snapshot', 'draft', 'reference', 'reference-verify', 'compact', 'reset-compact-failure', 'resume', 'verify'].includes(mode));
fs.mkdirSync(output, { recursive: true });
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
const pid = execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], { encoding: 'utf8' }).match(/^p(\d+)$/m)?.[1];
assert.ok(pid, 'QA app must be listening');
assert.equal(execFileSync('ps', ['-p', pid, '-o', 'comm='], { encoding: 'utf8' }).trim(), executable);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], { encoding: 'utf8' }));
assert.equal(plist.CFBundleIdentifier, 'com.novnski.luna.qa.delegation-ux');
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, path.join(output, 'data'));
assert.equal(plist.LSEnvironment.LUNA_QA_MCP_ISOLATION, '1');
const save = (name, value) => fs.writeFileSync(path.join(output, `${name}.json`), JSON.stringify(value, null, 2) + '\n');
const manifestPath = path.join(output, 'manifest.json');
save('app-identity', { checkedAt: new Date().toISOString(), pid: Number(pid), executable, port,
  bundleId: plist.CFBundleIdentifier, dataDir: plist.LSEnvironment.LUNA_DATA_DIR, mcpIsolation: true,
  signatureVerified: true, binarySha256: crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex') });
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
const call = (method, params = {}, timeoutMs = 45000) => new Promise((resolve, reject) => {
  const id = ++seq;
  const timeout = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timeout; inspect its receipt before retrying`)); }, timeoutMs);
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
const entries = value => value.frame?.reset ?? value.reset ?? [];
const rows = value => Array.isArray(value) ? value : value.chats ?? value.sessions ?? value.items ?? [];
const text = entry => (entry.parts || []).filter(part => part.kind === 'text').map(part => part.text).join('\n');
const assistant = (messages, marker) => messages.find(entry => entry.role === 'assistant' && text(entry).includes(marker));
const calledAfter = (messages, promptPrefix, tool) => {
  const at = messages.findLastIndex(entry => entry.role === 'user' && text(entry).startsWith(promptPrefix));
  return at >= 0 && messages.slice(at + 1).flatMap(entry => entry.parts ?? []).some(part =>
    part.kind === 'tool' && part.call?.kind === 'mcp' && part.call.server === 'luna' && part.call.tool === tool && !part.isError);
};
const selection = model => ({ model: model.id,
  reasoning: model.reasoningLevels.includes('high') ? 'high' : model.reasoningLevels[0] ?? null,
  modelOptions: Object.fromEntries(model.options.map(option => [option.id, option.defaultChoice])),
});
let manifest;
try {
  if (mode === 'seed') {
    assert.ok(!fs.existsSync(manifestPath), 'Reuse the existing manifest');
    const info = await call('EngineInfo');
    const catalogs = {};
    for (const provider of ['codex', 'claude-code']) {
      catalogs[provider] = await call('ListModels', { harness: provider }); save(`catalog-${provider}`, catalogs[provider]);
    }
    const models = {
      codex: catalogs.codex.find(model => model.id === 'gpt-6.1-sol') ?? catalogs.codex[0],
      'claude-code': catalogs['claude-code'].find(model => /sonnet/i.test(model.id)) ?? catalogs['claude-code'][0],
    };
    assert.ok(models.codex && models['claude-code']);
    manifest = { deviceId: info.deviceId, profileId: info.profileId, parents: {}, spaces: {}, phases: {} };
    // Save before writes, so any unknown creation outcome remains inspectable.
    save('manifest', manifest);
    for (const name of ['API project', 'Other project']) {
      const project = path.join(output, name.replaceAll(' ', '-').toLowerCase()); fs.mkdirSync(project, { recursive: true });
      fs.writeFileSync(path.join(project, 'README.md'), 'Disposable LUNA delegation QA. No external services or repository edits.\n');
      const space = crypto.randomUUID(); manifest.spaces[name] = { id: space, path: project }; save('manifest', manifest);
      await call('Mutate', { op: 'createSpace', spaceId: space, deviceId: info.deviceId, path: project });
    }
    for (const [provider, model] of Object.entries(models)) {
      const id = crypto.randomUUID(); const space = manifest.spaces['API project']; manifest.parents[provider] = id; save('manifest', manifest);
      await call('Mutate', { op: 'createChat', chatId: id, spaceId: space.id, cwd: space.path, config: {
        harness: provider, ...selection(model), sandbox: 'danger-full-access', permissionMode: 'full', effectivePermissionMode: 'full',
      } });
      await call('Mutate', { op: 'renameChat', chatId: id, title: `Delegation QA · ${provider}` });
    }
    for (const name of Object.keys(manifest.spaces)) {
      const id = crypto.randomUUID(); const space = manifest.spaces[name];
      await call('Mutate', { op: 'createChat', chatId: id, spaceId: space.id, cwd: space.path, config: { harness: 'codex', ...selection(models.codex), sandbox: 'danger-full-access', permissionMode: 'full', effectivePermissionMode: 'full' } });
      await call('Mutate', { op: 'renameChat', chatId: id, title: 'Retry review shared title' });
      space.peer = id; save('manifest', manifest);
    }
    console.log(JSON.stringify({ phase: 'seeded', parents: manifest.parents }));
  } else {
    manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
    if (mode === 'draft') save('draft-readback', await call('GetComposerDraft', { taskId: manifest.parents.codex }));
    if (mode === 'reference') {
      assert.ok(!manifest.referenceChat, 'Reference chat already allocated; reuse its ID');
      const catalog = await call('ListModels', { harness: 'claude-code' });
      const model = catalog.find(model => /sonnet/i.test(model.id)) ?? catalog[0]; assert.ok(model);
      const id = crypto.randomUUID(); manifest.referenceChat = id; save('manifest', manifest);
      const space = manifest.spaces['API project'];
      await call('Mutate', { op: 'createChat', chatId: id, spaceId: space.id, cwd: space.path, config: {
        harness: 'claude-code', ...selection(model), sandbox: 'danger-full-access', permissionMode: 'full', effectivePermissionMode: 'full',
      } });
      await call('Mutate', { op: 'renameChat', chatId: id, title: 'Reference navigation QA' });
      console.log(JSON.stringify({ referenceChat: id, title: 'Reference navigation QA' }));
    }
    if (mode === 'reference-verify') {
      assert.ok(manifest.referenceChat);
      const messages = entries(await call('WatchDocMessages', { chatId: manifest.referenceChat }));
      const submitted = messages.find(entry => entry.role === 'user' && text(entry).startsWith('Navigation reference:'));
      assert.ok(submitted && text(submitted).includes(`/chat/${manifest.spaces['Other project'].peer}?workspace=`));
      assert.ok(assistant(messages, 'CHAT_REFERENCE_RECEIVED'));
      assert.ok(!messages.flatMap(entry => entry.parts ?? []).some(part => part.kind === 'tool'));
      save('submitted-reference', { passed: true, taskId: manifest.referenceChat, targetId: manifest.spaces['Other project'].peer, messages });
    }
    if (mode === 'reset-compact-failure') {
      // Explicit recovery for the recorded pre-fix QA failures only. Never retry an
      // unknown compact request. Clear the disposable context first, keeping
      // transcript/children and the failed receipt as historical evidence.
      const old = JSON.parse(fs.readFileSync(path.join(output, 'compact-receipt.json')));
      assert.equal(old.status, 'failed');
      assert.ok((old.reason.message.includes('thread/resume:') && old.reason.message.includes('already has an active writer')) || old.reason.message.includes('Codex compaction completion timed out'));
      assert.notEqual(manifest.phases.compactRecovery?.sourceCommandId, old.commandId, 'Recovery already submitted; inspect receipt');
      const taskId = manifest.parents.codex; const snapshot = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
      const commandId = crypto.randomUUID(); manifest.phases.compactRecovery = { commandId, sourceCommandId: old.commandId }; save('manifest', manifest);
      save(`compact-failed-${old.commandId}`, old);
      const result = await call('ExecuteTaskCommand', { commandId, actorId: manifest.deviceId, clientId: manifest.deviceId,
        clientSessionId: crypto.randomUUID(), taskId, intent: { kind: 'clearContext' }, expectedTaskRevision: snapshot.taskRevision,
        ...(snapshot.bindingGeneration == null ? {} : { expectedBindingGeneration: snapshot.bindingGeneration }) });
      save(`compact-recovery-clear-${commandId}`, result); assert.equal(result.status, 'applied', JSON.stringify(result));
      manifest.phases.failedCompact = manifest.phases.compact; delete manifest.phases.compact; save('manifest', manifest);
      await call('QueueMessage', { chatId: taskId, messageId: `delegation-compact-recovery-ready-${commandId}`,
        text: 'Use only the attached LUNA app tools to discover your workspace and its tool guidance. Do not read files or create chats. Then reply exactly DELEGATION_COMPACT_READY.', holdForTurnEnd: true });
    }
    if (mode === 'start' || mode === 'resume') {
      assert.ok(!manifest.phases[mode], `${mode} was already submitted; inspect snapshot`);
      manifest.phases[mode] = { at: new Date().toISOString(), submitted: [] }; save('manifest', manifest);
      for (const [provider, parent] of Object.entries(manifest.parents)) {
        let prompt;
        if (mode === 'start') {
          const request = { prompt: 'Run exactly one harmless shell command: sleep 45. Do not read or modify files, access the network, or use other tools. After it finishes reply exactly DELEGATION_CHILD_DONE.',
            idempotencyKey: `delegation-${provider}`, title: `Retry review helper · ${provider}`, reportResults: true, continueParent: true };
          prompt = `Authorized LUNA E2E in a disposable project. Use only attached LUNA tools. Read get_workspace with topic delegation. Create exactly one background helper with ${JSON.stringify(request)}. Omit provider/model/reasoning/permission fields to inherit. Do not call wait_threads, poll, or wait for its result. Immediately after creation, independently compute 17 × 19 and reply exactly DELEGATION_PARENT_FREE 323, ending this turn. When the helper later reports completion, reply exactly DELEGATION_FOLLOWUP_DONE. Do not create any further helper or follow-up timer. This request authorizes only this helper; do not use external tools yourself.`;
        } else {
          prompt = `Authorized continuation of this disposable LUNA test. Discover the current app workspace and its built-in guidance using the attached tools, then report the available guidance topics. Create one idle transcript snapshot fork of THIS conversation with nativeContext:false, title "Retry review fork · ${provider}", and idempotencyKey "delegation-fork-${provider}". Use the current workspace revision and exact caller ID; preserve focus. Send that fork exactly one message: "Use the attached app tools to read your current workspace and built-in tool guidance. Do not read files, use external tools, or create more chats. Then reply exactly DELEGATION_FORK_TOOLS_OK." Use idempotencyKey "delegation-fork-send-${provider}". Do not wait for the fork. Reply exactly DELEGATION_RESUME_TOOLS_OK. Do not create another helper.`;
        }
        save(`request-${mode}-${provider}`, { parent, prompt });
        await call('QueueMessage', { chatId: parent, messageId: `delegation-${mode}-${provider}`, text: prompt, holdForTurnEnd: true });
        manifest.phases[mode].submitted.push(provider); save('manifest', manifest);
      }
    }
    if (mode === 'compact') {
      assert.ok(!manifest.phases.compact, 'Inspect recorded compaction receipt; do not retry blindly');
      const taskId = manifest.parents.codex; const snapshot = (await call('GetTaskCommandSnapshot', { taskId })).snapshot;
      const commandId = crypto.randomUUID(); manifest.phases.compact = { commandId, at: new Date().toISOString() }; save('manifest', manifest);
      save('runtime-before-compact', await call('GetRuntimeContract', { taskId }));
      const command = { commandId, actorId: manifest.deviceId, clientId: manifest.deviceId, clientSessionId: crypto.randomUUID(), taskId,
        intent: { kind: 'compactContext' }, expectedTaskRevision: snapshot.taskRevision,
        ...(snapshot.bindingGeneration == null ? {} : { expectedBindingGeneration: snapshot.bindingGeneration }) };
      save('compact-command', command);
      const compactResult = await call('ExecuteTaskCommand', command, 600000); save('compact-result', compactResult);
      save('compact-receipt', await call('GetTaskCommandReceipt', { commandId }));
      save('runtime-after-compact', await call('GetRuntimeContract', { taskId }));
      assert.equal(compactResult.status, 'applied', JSON.stringify(compactResult));
    }
    const chats = rows(await call('WatchChats')); const sessions = rows(await call('WatchSessions'));
    const result = {};
    for (const [provider, parentId] of Object.entries(manifest.parents)) {
      const parent = chats.find(chat => chat.id === parentId);
      const messages = entries(await call('WatchDocMessages', { chatId: parentId }));
      const children = [];
      for (const chat of chats.filter(chat => chat.childThread?.parentTaskId === parentId)) children.push({ chat, session: sessions.find(row => row.chatId === chat.id), messages: entries(await call('WatchDocMessages', { chatId: chat.id })) });
      const forks = [];
      for (const fork of (await call('ListSideChats', { parentId })).items) forks.push({ fork, messages: entries(await call('WatchDocMessages', { chatId: fork.id })), session: sessions.find(row => row.chatId === fork.id), runtime: await call('GetRuntimeContract', { taskId: fork.id }) });
      const approvals = messages.flatMap(entry => entry.parts ?? []).filter(part => part.kind === 'input' && part.questions?.some(question => question.approval));
      result[provider] = { parent, messages, children, forks, session: sessions.find(row => row.chatId === parentId), approvalCount: approvals.length,
        runtime: await call('GetRuntimeContract', { taskId: parentId }) };
      if (mode === 'verify') {
        assert.equal(approvals.length, 0); assert.equal(children.length, 1, `${provider}: exact child count`);
        const free = assistant(messages, 'DELEGATION_PARENT_FREE 323'); const done = assistant(children[0].messages, 'DELEGATION_CHILD_DONE');
        assert.ok(free && done && assistant(messages, 'DELEGATION_FOLLOWUP_DONE'), `${provider}: missing background result`);
        assert.ok(free.createdAt < done.createdAt, `${provider}: parent must finish while child is running`);
        assert.ok(assistant(messages, 'DELEGATION_RESUME_TOOLS_OK'), `${provider}: resumed tool discovery`);
        assert.equal(forks.length, 1); assert.ok(assistant(forks[0].messages, 'DELEGATION_FORK_TOOLS_OK'), `${provider}: fork tool discovery`);
        assert.ok(calledAfter(messages, 'Authorized continuation of this disposable LUNA test.', 'get_workspace'), `${provider}: actual post-boundary workspace call`);
        assert.ok(calledAfter(forks[0].messages, 'Use the attached app tools to read your current workspace', 'get_workspace'), `${provider}: actual fork workspace call`);
        assert.equal(forks[0].runtime.effective?.threadId, forks[0].fork.id, `${provider}: fork runtime identity`);
        const core = result[provider].runtime.effective?.instructions.find(section => section.name === 'LUNA core');
        assert.ok(core && core.characterCount <= 700, `${provider}: bounded persistent bootstrap`);
        assert.ok(children[0].chat.childThread.outcomes.some(outcome => outcome.completedAtMs > 0), `${provider}: stable completion clock`);
        assert.equal(messages.filter(entry => entry.id.startsWith(`child-report:${children[0].chat.id}:`) && entry.id.endsWith(':turn-completed')).length, 1, `${provider}: exactly one continuation`);
      }
    }
    save(`snapshot-${mode}-${Date.now()}`, result);
    const summary = Object.fromEntries(Object.entries(result).map(([provider, value]) => [provider, {
      parent: value.parent.id, status: value.session?.status, approvalCount: value.approvalCount,
      free: Boolean(assistant(value.messages, 'DELEGATION_PARENT_FREE 323')), followup: Boolean(assistant(value.messages, 'DELEGATION_FOLLOWUP_DONE')),
      resumed: Boolean(assistant(value.messages, 'DELEGATION_RESUME_TOOLS_OK')),
      children: value.children.map(child => ({ id: child.chat.id, status: child.session?.status, completed: Boolean(assistant(child.messages, 'DELEGATION_CHILD_DONE')) })),
      forks: value.forks.map(fork => ({ id: fork.fork.id, status: fork.session?.status, tools: Boolean(assistant(fork.messages, 'DELEGATION_FORK_TOOLS_OK')) })),
    }]));
    save('latest-summary', summary); console.log(JSON.stringify(summary));
    if (mode === 'verify') {
      const compact = JSON.parse(fs.readFileSync(path.join(output, 'compact-receipt.json')));
      const compactCommand = JSON.parse(fs.readFileSync(path.join(output, 'compact-command.json')));
      assert.equal(compact.status, 'applied');
      assert.equal(compact.snapshot.bindingGeneration, compactCommand.expectedBindingGeneration);
      fs.writeFileSync(path.join(output, 'native.pass'), 'passed\n');
    }
  }
} finally { ws.close(); }
