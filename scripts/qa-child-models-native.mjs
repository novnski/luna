#!/usr/bin/env node
// Drives only this task's isolated, signed QA app. Provider invocations use
// real parent LUNA tools; approvals are deliberately left for native UI QA.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
const root = path.resolve(import.meta.dirname, '..');
const output = path.join(root, 'target/qa/chat-placement-models/native');
const bundle = path.join(root, 'target/luna-macos-dev/LUNA QA Chat Placement.app');
const port = 59794;
const mode = process.argv[2] || 'snapshot';
assert.ok(['seed', 'start-codex', 'start-claude', 'snapshot', 'verify', 'verify-placement', 'verify-reversed'].includes(mode));
fs.mkdirSync(output, { recursive: true });
const executable = fs.realpathSync(path.join(bundle, 'Contents/MacOS/luna'));
execFileSync('codesign', ['--verify', '--deep', '--strict', bundle]);
const pid = execFileSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fp'], { encoding: 'utf8' }).match(/^p(\d+)$/m)?.[1];
assert.ok(pid, 'QA app is not listening');
assert.equal(execFileSync('ps', ['-p', pid, '-o', 'comm='], { encoding: 'utf8' }).trim(), executable);
const plist = JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', path.join(bundle, 'Contents/Info.plist')], { encoding: 'utf8' }));
assert.equal(plist.CFBundleIdentifier, 'com.novnski.luna.qa.chat-placement');
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR, path.join(output, 'data'));
const ws = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
const call = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++seq;
  const timeout = setTimeout(() => { ws.removeEventListener('message', receive); reject(new Error(`${method} timeout`)); }, 45000);
  function receive(event) {
    for (const line of String(event.data).split('\n').filter(Boolean)) {
      const frame = JSON.parse(line);
      if (frame.id !== id || !['item','ok','err'].some(key => Object.hasOwn(frame,key))) continue;
      clearTimeout(timeout); ws.removeEventListener('message', receive);
      if (frame.err) reject(new Error(JSON.stringify(frame.err)));
      else { if (Object.hasOwn(frame,'item')) ws.send(JSON.stringify({id,cancel:true})); resolve(Object.hasOwn(frame,'item')?frame.item:frame.ok); }
    }
  }
  ws.addEventListener('message', receive); ws.send(JSON.stringify({id,method,params}));
});
const save = (name, value) => fs.writeFileSync(path.join(output, `${name}.json`), JSON.stringify(value, null, 2)+'\n');
const manifestPath = path.join(output, 'manifest.json');
const text = entry => (entry.parts || []).filter(part => part.kind === 'text').map(part => part.text).join('\n');
const entries = value => value.frame?.reset ?? value.reset ?? [];
let manifest;
try {
  if (mode === 'seed') {
    assert.ok(!fs.existsSync(manifestPath), 'already seeded; use the saved manifest');
    const info = await call('EngineInfo');
    const catalogs = {};
    for (const provider of ['codex', 'claude-code']) {
      catalogs[provider] = await call('ListModels', { harness: provider });
      save(`catalog-${provider}`, catalogs[provider]);
    }
    const codex = catalogs.codex.find(model => model.id === 'gpt-6.1-sol') || catalogs.codex[0];
    const claude = catalogs['claude-code'].find(model => model.id.includes('sonnet')) || catalogs['claude-code'][0];
    assert.ok(codex && claude, 'both providers need a model catalog');
    const selection = model => {
      const option = model.options.find(option => option.choices.some(choice => choice.id === option.defaultChoice));
      return { model: model.id, reasoning: model.reasoningLevels.includes('high') ? 'high' : model.reasoningLevels[0],
        modelOptions: option ? { [option.id]: option.defaultChoice } : {} };
    };
    const project = path.join(output, 'project'); fs.mkdirSync(project);
    fs.writeFileSync(path.join(project,'README.md'), 'Isolated child model and chat placement QA. Do not modify files.\n');
    execFileSync('git',['init','-q',project]);
    const space = crypto.randomUUID();
    await call('Mutate',{op:'createSpace',spaceId:space,deviceId:info.deviceId,path:project});
    manifest = { project, space, deviceId:info.deviceId, parents:{}, selections:{codex:selection(codex),'claude-code':selection(claude)}, children:{}, binarySha256:crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex') };
    for (const provider of ['codex','claude-code']) {
      const id = crypto.randomUUID(); manifest.parents[provider] = id;
      await call('Mutate',{op:'createChat',chatId:id,spaceId:space,cwd:project,config:{harness:provider,...manifest.selections[provider],sandbox:'workspace-write',permissionMode:'ask',effectivePermissionMode:'ask'}});
      await call('Mutate',{op:'renameChat',chatId:id,title:provider === 'codex' ? 'QA Codex parent' : 'QA Claude parent'});
    }
    save('manifest',manifest); console.log(JSON.stringify(manifest));
  } else {
    manifest = JSON.parse(fs.readFileSync(manifestPath,'utf8'));
    if (mode.startsWith('start-')) {
      const provider = mode === 'start-codex' ? 'codex' : 'claude-code';
      const target = provider === 'codex' ? 'claude-code' : 'codex';
      const request = {prompt:`Reply exactly CHILD_${target === 'codex' ? 'CODEX' : 'CLAUDE'}_MODEL_QA_OK. Do not use tools or read or change files.`,idempotencyKey:`native-child-${target}`,title:target === 'codex'?'QA Codex child':'QA Claude child',provider:target,...manifest.selections[target],permissionMode:'ask',reportResults:false,continueParent:false};
      const prompt = `This is an isolated LUNA end-to-end test. Use only the LUNA tools named here. First call list_child_models for provider ${target} and query ${JSON.stringify(request.model)} to inspect its model options. Then call create_thread exactly once with this exact JSON: ${JSON.stringify(request)}. Do not use shell/file tools, send other chat messages, or change the requested provider/model/options. Wait for the local approval. Once the tool returns, report the created child ID and configured execution selection and stop. Reuse the same idempotency key if transport recovery is needed; never create another child.`;
      const messageId = `native-parent-${target}`;
      await call('QueueMessage',{chatId:manifest.parents[provider],messageId,text:prompt,holdForTurnEnd:true});
      save(`requested-${provider}`, {parent:manifest.parents[provider],request,messageId,prompt});
      console.log(JSON.stringify({parent:manifest.parents[provider],target,request,approval:'waiting for native UI'}));
    }
    const chats = await call('WatchChats'); const sessions = await call('WatchSessions');
    const snapshots = {};
    for (const provider of ['codex','claude-code']) {
      const parent = manifest.parents[provider];
      const children = chats.filter(chat => chat.childThread?.parentTaskId === parent);
      const parentMessages = entries(await call('WatchDocMessages',{chatId:parent}));
      snapshots[provider] = {parent:chats.find(chat=>chat.id===parent),parentMessages,children:[]};
      for (const child of children) {
        const childMessages = entries(await call('WatchDocMessages',{chatId:child.id}));
        const identity = await call('GetTaskIdentity',{taskId:child.id});
        snapshots[provider].children.push({chat:child,messages:childMessages,identity,session:sessions.find(session=>session.chatId===child.id)});
      }
    }
    save(`snapshot-${Date.now()}`,snapshots);
    if (mode.startsWith('verify')) {
      for (const provider of ['codex','claude-code']) {
        const target = provider === 'codex' ? 'claude-code' : 'codex';
        const selected = manifest.selections[target]; const rows = snapshots[provider].children;
        assert.equal(rows.length,1,`${provider} must have exactly one child`);
        const {chat,messages,identity} = rows[0];
        assert.equal(chat.config.harness,target); assert.equal(chat.config.model,selected.model);
        assert.equal(chat.config.reasoning,selected.reasoning); assert.deepEqual(chat.config.modelOptions,selected.modelOptions);
        assert.equal(identity.currentBinding.providerId,target);
        const users=messages.filter(entry=>entry.role==='user');assert.equal(users.length,1);
        assert.equal(text(users[0]),chat.childThread.initialPrompt);
        assert.ok(messages.some(entry=>entry.role==='assistant'&&text(entry).includes(`CHILD_${target === 'codex' ? 'CODEX' : 'CLAUDE'}_MODEL_QA_OK`)));
        assert.equal(chat.childThread.provisioning.createdBy,'agent');
        manifest.children[target]=chat.id;
      }
      let placement;
      if (mode !== 'verify') {
        const info = await call('EngineInfo');
        assert.ok(info.profileId, 'resolved profile ID is required');
        const settings = JSON.parse(fs.readFileSync(path.join(output,'data/ui-settings.json'),'utf8'));
        placement = settings.chatPlacements?.[info.profileId] ?? {promoted:[],docked:{}};
        const expected = mode === 'verify-placement'
          ? {promoted:[manifest.children.codex],docked:{[manifest.parents.codex]:manifest.parents['claude-code']}}
          : {promoted:[],docked:{}};
        assert.deepEqual(placement,expected);
        for (const parent of Object.values(snapshots)) assert.ok(!parent.parent.childThread, 'docking must not reparent an ordinary chat');
      }
      save('manifest',manifest); save(mode === 'verify' ? 'verified' : mode,{passed:true,manifest,snapshots,placement,checkedAt:new Date().toISOString()});
      console.log(JSON.stringify({passed:true,children:manifest.children,selections:manifest.selections,placement}));
    } else if (mode === 'snapshot') {
      console.log(JSON.stringify(Object.fromEntries(Object.entries(snapshots).map(([provider,value])=>[provider,{parent:value.parent?.id,children:value.children.map(child=>({id:child.chat.id,provider:child.chat.config.harness,model:child.chat.config.model,status:child.session?.status,messages:child.messages.length})),pending:value.parentMessages.flatMap(entry=>entry.parts||[]).filter(part=>part.kind==='input'&&!part.resolved).length}]))));
    }
  }
} finally { ws.close(); }
