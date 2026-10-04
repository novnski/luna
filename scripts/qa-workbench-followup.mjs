#!/usr/bin/env node
// Guarded native acceptance for the isolated follow-up QA app. The UI creates
// Main and its split child, chooses providers and sends both safe QA prompts.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';

const root=path.resolve(import.meta.dirname,'..');
const output=path.join(root,'target/qa/workbench-followup-2026-10-04/native');
const bundle=path.join(root,'target/luna-macos-dev/LUNA QA Workbench Followup.app');
const port=59806;
const mode=process.argv[2]??'snapshot';
assert.ok(['seed-project','adopt-main','verify-split','checkpoint-choice','verify-choice','verify-delivery','catalog-controls','checkpoint-restart','verify-restart','snapshot'].includes(mode));
fs.mkdirSync(output,{recursive:true});
const save=(name,value)=>fs.writeFileSync(path.join(output,`${name}.json`),JSON.stringify(value,null,2)+'\n');
const executable=fs.realpathSync(path.join(bundle,'Contents/MacOS/luna'));
execFileSync('codesign',['--verify','--deep','--strict',bundle]);
const pid=execFileSync('lsof',['-nP',`-iTCP:${port}`,'-sTCP:LISTEN','-Fp'],{encoding:'utf8'}).match(/^p(\d+)$/m)?.[1];
assert.ok(pid,'QA listener absent');
assert.equal(execFileSync('ps',['-p',pid,'-o','comm='],{encoding:'utf8'}).trim(),executable);
const plist=JSON.parse(execFileSync('plutil',['-convert','json','-o','-',path.join(bundle,'Contents/Info.plist')],{encoding:'utf8'}));
assert.equal(plist.CFBundleIdentifier,'com.novnski.luna.qa.workbench-followup');
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
const manifestPath=path.join(output,'manifest.json');
const text=entry=>(entry.parts??[]).filter(part=>part.kind==='text').map(part=>part.text).join('\n');
const entries=value=>value.frame?.reset??value.reset??[];
const panes=node=>node.kind==='pane'?[node]:[...panes(node.first),...panes(node.second)];
const binding=view=>view?.currentBinding??null;
const draftContent=view=>{
  const draft=view?.draft;
  return draft?Object.fromEntries(['taskId','text','inputItems','attachments','appshots','context'].map(key=>[key,draft[key]??null])):null;
};
try{
  const info=await call('EngineInfo');
  assert.ok(info.capabilities.includes('chat-workbench-v1'));
  if(mode==='seed-project'){
    assert.ok(!fs.existsSync(manifestPath),'Already seeded; reuse this profile and manifest.');
    const project=path.join(output,'project');
    fs.mkdirSync(project);
    execFileSync('git',['init','-q',project]);
    fs.writeFileSync(path.join(project,'README.md'),'Isolated workbench follow-up QA. Do not change files.\n');
    execFileSync('git',['-C',project,'add','README.md']);
    execFileSync('git',['-C',project,'-c','user.name=LUNA QA','-c','user.email=qa@example.test','commit','-qm','Initialize follow-up QA']);
    const space=crypto.randomUUID();
    await call('Mutate',{op:'createSpace',spaceId:space,deviceId:info.deviceId,path:project});
    save('manifest',{space,project,profileId:info.profileId,deviceId:info.deviceId});
    console.log('Project seeded; create Main with its first prompt through the native UI.');
  }else{
    const manifest=JSON.parse(fs.readFileSync(manifestPath,'utf8'));
    assert.equal(info.profileId,manifest.profileId);
    const rows=(await call('WatchChats')).filter(row=>row.spaceId===manifest.space);
    if(mode==='adopt-main'){
      const candidates=rows.filter(row=>!row.childThread&&!row.sideChatParentId);
      assert.equal(candidates.length,1,'Create exactly one Main through the native UI.');
      manifest.main=candidates[0].id;
      const messages=entries(await call('WatchDocMessages',{chatId:manifest.main}));
      assert.ok(messages.some(entry=>entry.role==='assistant'&&text(entry).includes('FOLLOWUP_MAIN_OK')));
      await call('Mutate',{op:'renameChat',chatId:manifest.main,title:'QA Followup Main'});
      manifest.mainMessages=messages;manifest.mainProvider=candidates[0].config.harness;
      save('manifest',manifest);
    }
    assert.ok(manifest.main,'Adopt the UI-created Main first.');
    const main=rows.find(row=>row.id===manifest.main);assert.ok(main);
    const children=rows.filter(row=>row.childThread?.parentTaskId===manifest.main);
    const layout=await call('GetChatWorkbench',{mainChatId:manifest.main});
    const mainMessages=entries(await call('WatchDocMessages',{chatId:manifest.main}));
    const childStates=[];
    for(const row of children){
      childStates.push({row,messages:entries(await call('WatchDocMessages',{chatId:row.id})),
        draft:await call('GetComposerDraft',{taskId:row.id}),identity:await call('GetTaskIdentity',{taskId:row.id})});
    }
    const snapshot={checkedAt:new Date().toISOString(),pid:Number(pid),profileId:info.profileId,
      binarySha256:crypto.createHash('sha256').update(fs.readFileSync(executable)).digest('hex'),
      main,mainMessages,layout,children:childStates};
    if(mode==='verify-split'){
      assert.equal(children.length,1,'One split click creates exactly one child.');
      const child=childStates[0];
      assert.equal(panes(layout.root).length,2);
      assert.ok(panes(layout.root).some(pane=>pane.tabs.some(tab=>tab.chatId===child.row.id)));
      assert.equal(child.messages.length,0);assert.equal(binding(child.identity),null);
      assert.equal(child.row.childThread.initialPrompt,'');
      assert.equal(child.row.childThread.provisioning.createdBy,'user');
      assert.deepEqual(mainMessages,manifest.mainMessages);
      manifest.child=child.row.id;manifest.initialChildProvider=child.row.config.harness;
      manifest.childLineage=child.row.childThread;save('manifest',manifest);save('verified-split',snapshot);
    }
    const child=childStates.find(child=>child.row.id===manifest.child);
    if(mode==='catalog-controls'){
      assert.ok(child);
      const source=child.row.config.harness;
      const target=source==='claude-code'?'codex':'claude-code';
      const account=child.row.config.providerAccountId;
      assert.ok(account,'Reproduce the source account passed by the picker.');
      let foreignError=null;
      try{await call('ListModels',{harness:target,accountId:account});}
      catch(error){foreignError=error.message;}
      assert.ok(foreignError,'A foreign provider account must remain invalid.');
      const models=await call('ListModels',{harness:target});
      assert.ok(models.length>0,'The target provider default account must return models.');
      save('verified-catalog-controls',{source,target,foreignError,
        defaultAccountModels:models.map(model=>({id:model.id,name:model.name})),
        childMessages:child.messages.length,childBinding:binding(child.identity),
        draft:draftContent(child.draft)});
    }
    if(mode==='checkpoint-choice'){
      assert.ok(child);assert.equal(child.messages.length,0);assert.equal(binding(child.identity),null);
      assert.ok(child.draft.draft?.text.includes('FOLLOWUP_CHILD_OK'));
      save('before-choice',snapshot);
    }
    if(mode==='verify-choice'){
      assert.ok(child);
      const before=JSON.parse(fs.readFileSync(path.join(output,'before-choice.json'),'utf8'));
      const original=before.children.find(item=>item.row.id===manifest.child);
      assert.notEqual(child.row.config.harness,manifest.initialChildProvider);
      assert.equal(main.config.harness,manifest.mainProvider);
      assert.equal(child.messages.length,0);assert.equal(binding(child.identity),null);
      assert.deepEqual(child.row.childThread,manifest.childLineage);
      assert.deepEqual(draftContent(child.draft),draftContent(original.draft));
      assert.deepEqual(mainMessages,manifest.mainMessages);
      save('verified-choice',snapshot);
    }
    if(mode==='verify-delivery'){
      assert.ok(child);
      assert.ok(child.messages.some(entry=>entry.role==='assistant'&&text(entry).includes('FOLLOWUP_CHILD_OK')));
      assert.notEqual(child.row.config.harness,manifest.initialChildProvider);
      assert.equal(binding(child.identity)?.providerId,child.row.config.harness);
      assert.equal(binding(child.identity)?.generation,1);
      assert.ok(binding(child.identity)?.providerTaskId);
      assert.deepEqual(child.row.childThread,manifest.childLineage);
      assert.deepEqual(mainMessages,manifest.mainMessages);
      assert.equal(main.config.harness,manifest.mainProvider);
      save('verified-delivery',snapshot);
    }
    if(mode==='checkpoint-restart')save('before-restart',snapshot);
    if(mode==='verify-restart'){
      const before=JSON.parse(fs.readFileSync(path.join(output,'before-restart.json'),'utf8'));
      const stable=value=>({layout:value.layout.root,focusedPaneId:value.layout.focusedPaneId,
        mainMessages:value.mainMessages,children:value.children.map(child=>({id:child.row.id,
          config:child.row.config,lineage:child.row.childThread,messages:child.messages,
          draft:draftContent(child.draft),binding:binding(child.identity)}))});
      assert.notEqual(snapshot.pid,before.pid);
      assert.deepEqual(stable(snapshot),stable(before));save('verified-restart',snapshot);
    }
    save(`snapshot-${Date.now()}`,snapshot);
    console.log(JSON.stringify({mode,pid:Number(pid),main:manifest.main,mainProvider:main.config.harness,
      panes:panes(layout.root).length,children:childStates.map(child=>({id:child.row.id,
        provider:child.row.config.harness,messages:child.messages.length,bindingGeneration:binding(child.identity)?.generation??null,
        draft:child.draft.draft?.text??''}))}));
  }
}finally{ws.close();}
