#!/usr/bin/env node
// Native UI acceptance readback, restricted to this disposable QA identity.
// The UI creates the conversations, selects kinds/providers and sends prompts.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';

const root=path.resolve(import.meta.dirname,'..');
const output=path.join(root,'target/qa/contained-ui-alignment/native');
const bundle=path.join(root,'target/luna-macos-dev/LUNA QA UI Alignment.app');
const port=59807;
const mode=process.argv[2]??'snapshot';
assert.ok(['seed-project','adopt-main','checkpoint-no-create','verify-no-create','verify-new-thread','verify-side','verify-context','verify-previous-tab','verify-next-tab','checkpoint-restart','verify-restart','snapshot'].includes(mode));
fs.mkdirSync(output,{recursive:true});
const save=(name,value)=>fs.writeFileSync(path.join(output,`${name}.json`),JSON.stringify(value,null,2)+'\n');
const read=name=>JSON.parse(fs.readFileSync(path.join(output,`${name}.json`),'utf8'));
const executable=fs.realpathSync(path.join(bundle,'Contents/MacOS/luna'));
execFileSync('codesign',['--verify','--deep','--strict',bundle]);
const pid=execFileSync('lsof',['-nP',`-iTCP:${port}`,'-sTCP:LISTEN','-Fp'],{encoding:'utf8'}).match(/^p(\d+)$/m)?.[1];
assert.ok(pid,'QA listener absent');
assert.equal(execFileSync('ps',['-p',pid,'-o','comm='],{encoding:'utf8'}).trim(),executable);
const plist=JSON.parse(execFileSync('plutil',['-convert','json','-o','-',path.join(bundle,'Contents/Info.plist')],{encoding:'utf8'}));
assert.equal(plist.CFBundleIdentifier,'com.novnski.luna.qa.ui-alignment');
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR,path.join(output,'data'));
const ws=new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((ok,fail)=>{ws.onopen=ok;ws.onerror=fail;});
let seq=0;
const call=(method,params={})=>new Promise((ok,fail)=>{
  const id=++seq;
  const timer=setTimeout(()=>{ws.removeEventListener('message',receive);fail(new Error(`${method} timeout`));},45000);
  function receive(event){
    for(const line of String(event.data).split('\n').filter(Boolean)){
      const frame=JSON.parse(line);
      if(frame.id!==id||!['item','ok','err'].some(key=>Object.hasOwn(frame,key)))continue;
      clearTimeout(timer);ws.removeEventListener('message',receive);
      if(frame.err)fail(new Error(JSON.stringify(frame.err)));
      else {if(Object.hasOwn(frame,'item'))ws.send(JSON.stringify({id,cancel:true}));ok(Object.hasOwn(frame,'item')?frame.item:frame.ok);}
    }
  }
  ws.addEventListener('message',receive);ws.send(JSON.stringify({id,method,params}));
});
const text=entry=>(entry.parts??[]).filter(part=>part.kind==='text').map(part=>part.text).join('\n');
const entries=value=>value.frame?.reset??value.reset??[];
const panes=node=>node.kind==='pane'?[node]:[...panes(node.first),...panes(node.second)];
const draftContent=view=>{
  const draft=view?.draft;
  return draft?Object.fromEntries(['taskId','text','inputItems','attachments','appshots','context'].map(key=>[key,draft[key]??null])):null;
};
const stableBinding=view=>{
  const binding=view?.currentBinding;
  return binding?Object.fromEntries(Object.entries(binding).filter(([key])=>!['reconciliation','lastObservedAtMs'].includes(key))):null;
};
try{
  const info=await call('EngineInfo');
  assert.ok(info.capabilities.includes('chat-workbench-v1'));
  if(mode==='seed-project'){
    assert.ok(!fs.existsSync(path.join(output,'manifest.json')),'Already seeded; reuse this profile.');
    const project=path.join(output,'project');
    fs.mkdirSync(project);
    execFileSync('git',['init','-q',project]);
    fs.writeFileSync(path.join(project,'README.md'),'Isolated contained UI acceptance. Do not change files.\n');
    execFileSync('git',['-C',project,'add','README.md']);
    execFileSync('git',['-C',project,'-c','user.name=LUNA QA','-c','user.email=qa@example.test','commit','-qm','Initialize contained UI QA']);
    const space=crypto.randomUUID();
    await call('Mutate',{op:'createSpace',spaceId:space,deviceId:info.deviceId,path:project});
    save('manifest',{space,project,profileId:info.profileId,deviceId:info.deviceId});
    console.log('Seeded only the QA project. Create Main through the native UI.');
  }else{
    const manifest=read('manifest');
    assert.equal(info.profileId,manifest.profileId);
    const rows=(await call('WatchChats')).filter(row=>row.spaceId===manifest.space);
    if(mode==='adopt-main'){
      const candidates=rows.filter(row=>!row.childThread&&!row.sideChatParentId&&!row.forkSourceTaskId);
      assert.equal(candidates.length,1,'Create exactly one Main through the UI.');
      manifest.main=candidates[0].id;
      const messages=entries(await call('WatchDocMessages',{chatId:manifest.main}));
      assert.ok(messages.some(entry=>entry.role==='assistant'&&text(entry).includes('CONTAINED_MAIN_OK')));
      assert.ok(messages.some(entry=>entry.role==='user'&&text(entry).includes('CONTEXT_ALIGNMENT_7631')));
      const sourceState=await call('GetTaskCommandSnapshot',{taskId:manifest.main});
      assert.ok(!sourceState.snapshot.activeTurnId,'Wait for Main to finish before capturing its baseline.');
      await call('Mutate',{op:'renameChat',chatId:manifest.main,title:'QA Alignment Main'});
      manifest.mainMessages=messages;manifest.mainProvider=candidates[0].config.harness;
      save('manifest',manifest);
    }
    assert.ok(manifest.main,'Adopt the UI-created Main first.');
    const chats=[];
    for(const row of rows){
      chats.push({row,messages:entries(await call('WatchDocMessages',{chatId:row.id})),
        draft:await call('GetComposerDraft',{taskId:row.id}),identity:await call('GetTaskIdentity',{taskId:row.id})});
    }
    chats.sort((a,b)=>a.row.id.localeCompare(b.row.id));
    const layout=await call('GetChatWorkbench',{mainChatId:manifest.main});
    const snapshot={checkedAt:new Date().toISOString(),pid:Number(pid),profileId:info.profileId,
      binarySha256:crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex'),layout,chats};
    if(mode==='verify-previous-tab'||mode==='verify-next-tab'){
      save(`${mode}-readback`,snapshot);
      const focused=panes(layout.root).find(pane=>pane.id===layout.focusedPaneId);
      assert.equal(focused?.activeChatId,mode==='verify-previous-tab'?manifest.child:manifest.side);
    }
    const main=chats.find(chat=>chat.row.id===manifest.main);assert.ok(main);
    assert.deepEqual(main.messages,manifest.mainMessages,'Creating or running another chat cannot modify Main history.');
    assert.equal(main.row.config.harness,manifest.mainProvider);
    const stable=value=>value.chats.map(chat=>({id:chat.row.id,config:chat.row.config,
      child:chat.row.childThread??null,source:chat.row.forkSourceTaskId??chat.row.sideChatParentId??null,
      messages:chat.messages,draft:draftContent(chat.draft),binding:stableBinding(chat.identity)}));
    if(mode==='checkpoint-no-create')save('before-chooser-cancel',snapshot);
    if(mode==='verify-no-create'){
      assert.deepEqual(stable(snapshot),stable(read('before-chooser-cancel')));
      save('verified-chooser-cancel',snapshot);
    }
    if(mode==='verify-new-thread'){
      const fresh=chats.filter(chat=>chat.row.childThread?.parentTaskId===manifest.main);
      assert.equal(fresh.length,1);const child=fresh[0];
      assert.notEqual(child.row.config.harness,manifest.mainProvider,'Choose another provider in the native creation menu.');
      assert.equal(child.messages.length,0);assert.equal(stableBinding(child.identity),null);
      assert.ok(!child.row.harnessSessionId);
      assert.equal(child.row.childThread.initialPrompt,'');
      assert.equal(child.row.childThread.provisioning.createdBy,'user');
      assert.ok(!child.row.forkSourceTaskId&&!child.row.sideChatParentId);
      assert.ok(panes(layout.root).some(pane=>pane.tabs.some(tab=>tab.chatId===child.row.id)));
      manifest.child=child.row.id;save('manifest',manifest);save('verified-new-thread',snapshot);
    }
    if(mode==='verify-side'||mode==='verify-context'){
      const sides=chats.filter(chat=>(chat.row.forkSourceTaskId??chat.row.sideChatParentId)===manifest.main);
      assert.equal(sides.length,1,'Create exactly one side chat from Main.');
      const side=sides[0];
      assert.equal(side.row.config.harness,manifest.mainProvider);
      assert.equal(side.row.sideChatContextMode,'nativeFork');
      assert.ok(!side.row.childThread);
      assert.equal(side.identity.task.lineage.sourceTaskId,manifest.main);
      assert.equal(side.identity.currentBinding.origin,'forked');
      assert.equal(side.identity.currentBinding.generation,1);
      assert.notEqual(side.identity.currentBinding.providerTaskId,main.identity.currentBinding.providerTaskId);
      assert.ok(panes(layout.root).some(pane=>pane.tabs.some(tab=>tab.chatId===side.row.id)));
      if(mode==='verify-side'){
        manifest.side=side.row.id;save('manifest',manifest);save('verified-native-side',snapshot);
      }else{
        assert.equal(side.row.id,manifest.side);
        assert.ok(side.messages.some(entry=>entry.role==='assistant'&&text(entry).includes('CONTEXT_ALIGNMENT_7631')));
        save('verified-captured-context',snapshot);
      }
    }
    if(mode==='checkpoint-restart')save('before-restart',snapshot);
    if(mode==='verify-restart'){
      const before=read('before-restart');assert.notEqual(snapshot.pid,before.pid);
      assert.deepEqual(stable(snapshot),stable(before));
      assert.deepEqual(snapshot.layout.root,before.layout.root);
      assert.equal(snapshot.layout.focusedPaneId,before.layout.focusedPaneId);
      save('verified-restart',snapshot);
    }
    save(`snapshot-${Date.now()}`,snapshot);
    console.log(JSON.stringify({mode,pid:Number(pid),main:manifest.main,panes:panes(layout.root).length,
      chats:chats.map(chat=>({id:chat.row.id,provider:chat.row.config.harness,messages:chat.messages.length,
        childOf:chat.row.childThread?.parentTaskId??null,sideOf:chat.row.forkSourceTaskId??chat.row.sideChatParentId??null,
        bindingGeneration:chat.identity.currentBinding?.generation??null}))}));
  }
}finally{ws.close();}
