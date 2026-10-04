#!/usr/bin/env node
// Native acceptance driver: only this task's isolated signed QA app/profile.
// Tab creation, dragging, closing, reopening and typing are performed in the UI.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';

const root=path.resolve(import.meta.dirname,'..');
const output=path.join(root,'target/qa/super-chat-workbench/native');
const bundle=path.join(root,'target/luna-macos-dev/LUNA QA Workbench.app');
const port=59805;
const mode=process.argv[2] ?? 'snapshot';
assert.ok(['seed','name-children','snapshot','verify-drafts','run-main','open-other','checkpoint-restart','verify-restart','verify'].includes(mode));
fs.mkdirSync(output,{recursive:true});
const executable=fs.realpathSync(path.join(bundle,'Contents/MacOS/luna'));
execFileSync('codesign',['--verify','--deep','--strict',bundle]);
const pid=execFileSync('lsof',['-nP',`-iTCP:${port}`,'-sTCP:LISTEN','-Fp'],{encoding:'utf8'}).match(/^p(\d+)$/m)?.[1];
assert.ok(pid,'QA listener is absent');
assert.equal(execFileSync('ps',['-p',pid,'-o','comm='],{encoding:'utf8'}).trim(),executable);
const plist=JSON.parse(execFileSync('plutil',['-convert','json','-o','-',path.join(bundle,'Contents/Info.plist')],{encoding:'utf8'}));
assert.equal(plist.CFBundleIdentifier,'com.novnski.luna.qa.workbench');
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR,path.join(output,'data'));
const ws=new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject;});
let seq=0;
const call=(method,params={})=>new Promise((resolve,reject)=>{
  const id=++seq;
  const timeout=setTimeout(()=>{ws.removeEventListener('message',receive);reject(new Error(`${method} timeout`));},45000);
  function receive(event){
    for(const line of String(event.data).split('\n').filter(Boolean)){
      const frame=JSON.parse(line);
      if(frame.id!==id||!['item','ok','err'].some(key=>Object.hasOwn(frame,key)))continue;
      clearTimeout(timeout);ws.removeEventListener('message',receive);
      if(frame.err)reject(new Error(JSON.stringify(frame.err)));
      else {if(Object.hasOwn(frame,'item'))ws.send(JSON.stringify({id,cancel:true}));resolve(Object.hasOwn(frame,'item')?frame.item:frame.ok);}
    }
  }
  ws.addEventListener('message',receive);ws.send(JSON.stringify({id,method,params}));
});
const save=(name,value)=>fs.writeFileSync(path.join(output,`${name}.json`),JSON.stringify(value,null,2)+'\n');
const manifestPath=path.join(output,'manifest.json');
const entries=value=>value.frame?.reset??value.reset??[];
const messageText=entry=>(entry.parts??[]).filter(part=>part.kind==='text').map(part=>part.text).join('\n');
const tabs=node=>node.kind==='pane'?node.tabs:[...tabs(node.first),...tabs(node.second)];
const fields=(value,keys)=>value?Object.fromEntries(keys.map(key=>[key,value[key]??null])):null;
const bindingFields=['bindingId','taskId','generation','providerId','providerInstanceId','providerTaskId','workspaceId','worktreeId'];
// Ended side chats deliberately admit ReadSideChat, not generic task RPCs.
// Inspect only this guarded QA profile's immutable identity ledger, read-only.
function savedSideBinding(id){
  const database=path.join(output,'data/profiles/local/docs.sqlite3');
  assert.equal(fs.realpathSync(database),database);
  const ledger=JSON.parse(execFileSync('sqlite3',['-readonly',database,"select cast(bytes as text) from snapshots where doc_id='luna-task-identity-events-v1';"],{encoding:'utf8'}));
  assert.ok([1,2].includes(ledger.schemaVersion));
  const bindings=ledger.events.filter(event=>event.type==='bindingCreated'&&event.binding?.taskId===id).map(event=>event.binding);
  assert.equal(bindings.length,1,'The QA side fork must retain its one binding.');
  const binding=bindings[0];
  assert.ok(!ledger.events.some(event=>['bindingMaterialized','bindingReplaced'].includes(event.type)&&(event.binding_id===binding.bindingId||event.bindingId===binding.bindingId)), 'Unexpected binding rewrite requires an explicit ledger review.');
  return fields(binding,bindingFields);
}
let manifest;
try{
  const info=await call('EngineInfo');
  assert.ok(info.capabilities.includes('chat-workbench-v1'));
  if(mode==='seed'){
    assert.ok(!fs.existsSync(manifestPath),'Already seeded; keep the existing manifest.');
    const catalog=await call('ListModels',{harness:'codex'});save('model-catalog',catalog);
    const model=catalog.find(item=>item.id==='gpt-6.1-sol')??catalog[0];assert.ok(model);
    const modelOptions={};
    for(const option of model.options)if(option.id==='serviceTier'&&option.choices.some(choice=>choice.id==='default'))modelOptions.serviceTier='default';
    const config={harness:'codex',model:model.id,reasoning:model.reasoningLevels.includes('high')?'high':model.reasoningLevels[0],modelOptions,sandbox:'workspace-write',permissionMode:'ask',effectivePermissionMode:'ask'};
    const space=crypto.randomUUID(),main=crypto.randomUUID(),other=crypto.randomUUID();
    const project=path.join(output,'project');const otherProject=path.join(output,'other-project');
    for(const [directory,name] of [[project,'main'],[otherProject,'other']]){
      fs.mkdirSync(directory);execFileSync('git',['init','-q',directory]);
      fs.writeFileSync(path.join(directory,'README.md'),`Isolated ${name} workbench QA. Do not change files.\n`);
      execFileSync('git',['-C',directory,'-c','user.name=LUNA QA','-c','user.email=qa@example.test','add','README.md']);
      execFileSync('git',['-C',directory,'-c','user.name=LUNA QA','-c','user.email=qa@example.test','commit','-qm','Initialize isolated workbench fixture']);
    }
    await call('Mutate',{op:'createSpace',spaceId:space,deviceId:info.deviceId,path:project});
    const otherSpace=crypto.randomUUID();await call('Mutate',{op:'createSpace',spaceId:otherSpace,deviceId:info.deviceId,path:otherProject});
    await call('Mutate',{op:'createChat',chatId:main,spaceId:space,cwd:project,config});
    await call('Mutate',{op:'renameChat',chatId:main,title:'QA Main workbench'});
    await call('Mutate',{op:'createChat',chatId:other,spaceId:otherSpace,cwd:otherProject,config});
    await call('Mutate',{op:'renameChat',chatId:other,title:'QA Other checkout'});
    manifest={main,other,space,otherSpace,project,otherProject,profileId:info.profileId,deviceId:info.deviceId,config,children:[],binarySha256:crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex')};
    save('manifest',manifest);console.log(JSON.stringify(manifest));
  }else{
    manifest=JSON.parse(fs.readFileSync(manifestPath,'utf8'));assert.equal(info.profileId,manifest.profileId);
    const chats=await call('WatchChats');
    const children=chats.filter(chat=>chat.childThread?.parentTaskId===manifest.main).sort((a,b)=>a.createdAt.localeCompare(b.createdAt)||a.id.localeCompare(b.id));
    if(mode==='name-children'){
      assert.ok(children.length>=2,'Create two child tabs with the real + button first.');
      for(const [index,child] of children.entries())await call('Mutate',{op:'renameChat',chatId:child.id,title:`QA Child ${String.fromCharCode(65+index)}`});
      manifest.children=children.map(child=>child.id);save('manifest',manifest);
    }
    if(mode==='run-main')await call('QueueMessage',{chatId:manifest.main,messageId:'workbench-main-sentinel',text:'Reply exactly MAIN_WORKBENCH_QA_OK. Do not use tools or read or change files.',holdForTurnEnd:true});
    let layout=await call('GetChatWorkbench',{mainChatId:manifest.main});
    if(mode==='open-other'){
      const changed=await call('MutateChatWorkbench',{mainChatId:manifest.main,profileId:manifest.profileId,expectedRevision:layout.revision,commandId:crypto.randomUUID(),command:{op:'openTab',tab:{chatId:manifest.other}}});
      assert.equal(changed.conflict,false);layout=changed.snapshot;
      const other=chats.find(chat=>chat.id===manifest.other);
      assert.ok(other&&!other.childThread,'An alongside view must not become a child.');
      assert.equal(other.cwd,manifest.otherProject);
    }
    const open=tabs(layout.root);assert.equal(open.filter(tab=>tab.chatId===manifest.main).length,1);
    assert.equal(new Set(open.map(tab=>tab.chatId)).size,open.length);
    const snapshots=[];
    for(const child of children){
      const messages=entries(await call('WatchDocMessages',{chatId:child.id}));
      const draft=await call('GetComposerDraft',{taskId:child.id});
      const identity=await call('GetTaskIdentity',{taskId:child.id});
      assert.equal(child.childThread.provisioning.createdBy,'user');
      assert.equal(child.childThread.initialPrompt,'');assert.equal(child.childThread.reportResults,false);assert.equal(child.childThread.continueParent,false);
      snapshots.push({chat:child,messages,draft,identity});
    }
    const sideHistory=await call('ListSideChats',{parentId:manifest.main});
    const sideHistoryByParent={[manifest.main]:sideHistory};
    for(const child of children)sideHistoryByParent[child.id]=await call('ListSideChats',{parentId:child.id});
    const allSides=Object.values(sideHistoryByParent).flatMap(history=>history.items);
    const sidePages=[];
    for(const side of allSides){
      const page=await call('ReadSideChat',{id:side.id});
      const identity=page.meta.state==='active'?await call('GetTaskIdentity',{taskId:side.id}):null;
      const binding=savedSideBinding(side.id);
      if(identity)assert.deepEqual(fields(identity.currentBinding,bindingFields),binding);
      sidePages.push({page,identity,binding,bindingReadback:'QA profile identity ledger, sqlite3 -readonly'});
    }
    const mainMessages=entries(await call('WatchDocMessages',{chatId:manifest.main}));
    const snapshot={checkedAt:new Date().toISOString(),pid:Number(pid),binarySha256:crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex'),bundleId:plist.CFBundleIdentifier,ipcPort:port,dataDir:plist.LSEnvironment.LUNA_DATA_DIR,layout,mainMessages,children:snapshots,sideHistory,sideHistoryByParent,sidePages};
    save(`snapshot-${Date.now()}`,snapshot);
    const stable=value=>({
      layout:{main:value.layout.mainChatId,root:value.layout.root,focusedPaneId:value.layout.focusedPaneId,closedTabs:value.layout.closedTabs},
      mainMessages:value.mainMessages.map(entry=>({id:entry.id,role:entry.role,text:messageText(entry)})),
      children:value.children.map(child=>({
        id:child.chat.id,lineage:child.identity.task.lineage,
        binding:fields(child.identity.currentBinding,['bindingId','taskId','generation','providerId','providerInstanceId','providerTaskId','workspaceId','worktreeId']),
        bindingIds:child.identity.bindings.map(binding=>binding.bindingId).sort(),
        draft:fields(child.draft.draft,['draftId','taskId','text','inputItems','attachments','appshots','context','execution']),
        messages:child.messages.map(entry=>({id:entry.id,role:entry.role,text:messageText(entry)})),
      })),
      sides:Object.values(value.sideHistoryByParent).flatMap(history=>history.items).map(side=>({id:side.id,parentId:side.parentId,state:side.state})).sort((a,b)=>a.id.localeCompare(b.id)),
      sidePages:value.sidePages.map(side=>({
        id:side.page.meta.id,contextMode:side.page.meta.contextMode,
        binding:side.binding,
        messages:side.page.entries.map(entry=>({id:entry.id,role:entry.role,text:messageText(entry)})),
      })).sort((a,b)=>a.id.localeCompare(b.id)),
    });
    if(mode==='checkpoint-restart')save('before-restart',snapshot);
    if(mode==='verify-restart'){
      const before=JSON.parse(fs.readFileSync(path.join(output,'before-restart.json'),'utf8'));
      assert.notEqual(snapshot.pid,before.pid,'Restart must replace the actual bundle process.');
      assert.deepEqual(stable(snapshot),stable(before),'A real restart must preserve the layout, drafts, bindings and messages.');
      save('verified-restart',snapshot);
    }
    if(mode==='verify-drafts'){
      assert.ok(snapshots.length>=2);
      for(const child of snapshots){assert.equal(child.messages.length,0);assert.ok(!child.identity.currentBinding);}
      assert.ok(snapshots.some(child=>child.draft.draft?.text==='WORKBENCH_DRAFT_REOPEN_QA'));
      save('verified-drafts',snapshot);
    }
    if(mode==='verify'){
      assert.ok(snapshots.length>=2);
      assert.ok(snapshots.some(child=>child.messages.some(entry=>entry.role==='assistant'&&messageText(entry).includes('CHILD_WORKBENCH_QA_OK'))));
      assert.ok(snapshots.some(child=>child.messages.length===0),'The other blank child must remain unstarted.');
      assert.ok(mainMessages.some(entry=>entry.role==='assistant'&&messageText(entry).includes('MAIN_WORKBENCH_QA_OK')));
      assert.ok(allSides.length>=1,'Exercise real side-chat creation/reopening in the UI.');
      assert.ok(allSides.some(side=>manifest.children.includes(side.parentId)),'Exercise a side fork from a child and reopen it from the Main library.');
      const fork=sidePages.find(side=>manifest.children.includes(side.page.meta.parentId)&&side.page.entries.some(entry=>entry.role==='assistant'&&messageText(entry).includes('SIDE_WORKBENCH_QA_OK')));
      assert.ok(fork,'The reopened side history must contain its own real assistant reply.');
      assert.equal(fork.page.meta.contextMode,'nativeFork');
      assert.equal(fork.page.meta.state,'ended','Exercise explicit End separately from closing its tab.');
      const parent=snapshots.find(child=>child.chat.id===fork.page.meta.parentId);
      assert.ok(fork.binding.providerTaskId);
      assert.ok(parent.identity.currentBinding?.providerTaskId);
      assert.notEqual(fork.binding.providerTaskId,parent.identity.currentBinding.providerTaskId,'The side fork must have its own provider conversation.');
      save('verified-native',snapshot);
    }
    console.log(JSON.stringify({mode,main:manifest.main,revision:layout.revision,open:open.map(tab=>tab.chatId),closed:layout.closedTabs.map(item=>item.tab.chatId),children:snapshots.map(child=>({id:child.chat.id,title:child.chat.title,messages:child.messages.length,draft:child.draft.draft?.text??'',provider:child.chat.config?.harness})),sides:allSides.map(side=>({id:side.id,state:side.state,parent:side.parentId}))}));
  }
}finally{ws.close();}
