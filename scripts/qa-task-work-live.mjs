#!/usr/bin/env node
// Signed disposable app -> real Codex/Claude -> truthful shared work and exact
// native stop. Every attempted send is saved first; failed/unknown sends are
// never replayed by this runner.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync} from 'node:child_process';
const root=path.resolve(import.meta.dirname,'..');
const base=path.join(root,'target/qa/harness-evolution/slice-3/native');
const bundle=path.join(root,'target/luna-macos-dev/LUNA QA Work.app');
const port=59812;
const mode=process.argv[2]??'run';
assert.ok(['run','verify-restart','finish-readback','inspect'].includes(mode));
execFileSync('codesign',['--verify','--deep','--strict',bundle]);
const plist=JSON.parse(execFileSync('plutil',['-convert','json','-o','-',path.join(bundle,'Contents/Info.plist')],{encoding:'utf8'}));
assert.equal(plist.CFBundleIdentifier,'com.novnski.luna.qa.work');
assert.equal(plist.LSEnvironment.LUNA_IPC_PORT,String(port));
assert.equal(plist.LSEnvironment.LUNA_DATA_DIR,path.join(base,'data'));
const pids=[...new Set(execFileSync('lsof',['-nP',`-iTCP:${port}`,'-sTCP:LISTEN','-Fp'],{encoding:'utf8'}).match(/^p\d+$/gm))];
assert.equal(pids.length,1);
const pid=Number(pids[0].slice(1));
assert.equal(execFileSync('ps',['-p',String(pid),'-o','comm='],{encoding:'utf8'}).trim(),fs.realpathSync(path.join(bundle,'Contents/MacOS/luna')));
fs.mkdirSync(base,{recursive:true});
const ws=new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject});
let seq=0;
function call(method,params={}){return new Promise((resolve,reject)=>{
 const id=++seq;const timer=setTimeout(()=>{ws.removeEventListener('message',receive);ws.send(JSON.stringify({id,cancel:true}));reject(new Error(`${method} timed out; no automatic replay`))},45000);
 function receive(event){for(const line of String(event.data).split('\n').filter(Boolean)){const frame=JSON.parse(line);if(frame.id!==id||!['ok','err','item'].some(key=>Object.hasOwn(frame,key)))continue;clearTimeout(timer);ws.removeEventListener('message',receive);if(Object.hasOwn(frame,'item'))ws.send(JSON.stringify({id,cancel:true}));if(frame.err)reject(new Error(JSON.stringify(frame.err)));else resolve(Object.hasOwn(frame,'item')?frame.item:frame.ok)}}
 ws.addEventListener('message',receive);ws.send(JSON.stringify({id,method,params}));
})}
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function until(fn,label,ms=150000){const end=Date.now()+ms;while(Date.now()<end){const value=await fn();if(value)return value;await delay(200)}throw new Error(`Timed out: ${label}`)}
const save=(name,value)=>fs.writeFileSync(path.join(evidence,`${name}.json`),`${JSON.stringify(value,null,2)}\n`);
const transcript=async taskId=>{const value=await call('WatchDocMessages',{chatId:taskId});return value.frame?.reset??value.reset??[]};
const work=taskId=>call('GetTaskWork',{taskId,limit:100});
const identity=taskId=>call('GetTaskIdentity',{taskId});
const config=async provider=>{
 const catalog=await call('ListModels',{harness:provider});save(`catalog-${provider}`,catalog);
 const preferred=provider==='codex'?['gpt-6.1-sol','gpt-6-luna']:['claude-sonnet-4-6','claude-sonnet-4-5'];
 const model=preferred.map(id=>catalog.find(m=>m.id===id)).find(Boolean)??catalog.find(m=>m.isDefault)??catalog[0];assert.ok(model);
 return {harness:provider,model:model.id,reasoning:model.reasoningLevels?.includes('low')?'low':model.reasoningLevels?.[0],permissionMode:'full',effectivePermissionMode:'full',sandbox:'danger-full-access'};
};
async function command(taskId,intent){const {snapshot}=await call('GetTaskCommandSnapshot',{taskId});return call('ExecuteTaskCommand',{taskId,commandId:crypto.randomUUID(),actorId:'qa-user',clientId:'qa-work',clientSessionId:'qa-work',expectedTaskRevision:snapshot.taskRevision,expectedBindingGeneration:snapshot.bindingGeneration,intent})}
let evidence;
async function finish(manifest){
 for(const [provider,taskId] of Object.entries(manifest.tasks)){
  const snapshot=await until(async()=>{const value=await work(taskId);return value.items.filter(i=>i.kind==='nativeChild'||i.kind==='shell').every(i=>['completed','failed','stopped'].includes(i.state))&&value},`${provider} terminal work`,90000);
  save(`${provider}-terminal`,snapshot);
  const entries=await transcript(taskId);save(`${provider}-transcript`,entries);
  assert.equal(entries.filter(e=>e.id===manifest.messages[provider]).length,1);
  if(provider==='claude-code'&&manifest.shellMessage)assert.equal(entries.filter(e=>e.id===manifest.shellMessage).length,1);
  const native=(await identity(taskId)).currentBinding?.providerTaskId;
  if(native)assert.ok(!JSON.stringify(snapshot).includes(native),'work view exposes native session ID');
 }
 if(manifest.service){const services=await call('ProjectCommandStatus',{chatId:manifest.tasks['claude-code']});save('service-final',services);if(services.runs.some(r=>r.id===manifest.service.id&&['pending','running'].includes(r.status)))save('service-stop',await call('StopProjectCommand',{chatId:manifest.tasks['claude-code'],runId:manifest.service.id}));}
 const status=execFileSync('git',['-C',manifest.project,'status','--porcelain=v1'],{encoding:'utf8'});assert.equal(status,'');
 manifest.completed=true;save('manifest',manifest);save('pass',{pid,port,tasks:manifest.tasks,completedAt:new Date().toISOString()});
 console.log(JSON.stringify({passed:true,evidence,tasks:manifest.tasks}));
}
try{
 if(mode==='run'){
  evidence=fs.mkdtempSync(path.join(base,'run-'));fs.writeFileSync(path.join(base,'latest'),evidence);
  const project=path.join(evidence,'fixture');fs.mkdirSync(project);execFileSync('git',['init','-q','-b','main',project]);execFileSync('git',['-C',project,'config','user.name','Work QA']);execFileSync('git',['-C',project,'config','user.email','qa@example.invalid']);fs.writeFileSync(path.join(project,'README.md'),'Disposable bounded work fixture.\n');execFileSync('git',['-C',project,'add','README.md']);execFileSync('git',['-C',project,'commit','-q','-m','fixture']);
  const engine=await call('EngineInfo');const projectId=crypto.randomUUID();await call('Mutate',{op:'createSpace',spaceId:projectId,deviceId:engine.deviceId,path:project});
  const manifest={project,projectId,tasks:{},messages:{},startedAt:new Date().toISOString(),pid,port};save('manifest',manifest);
  for(const provider of ['codex','claude-code']){
   const taskId=crypto.randomUUID();manifest.tasks[provider]=taskId;save('manifest',manifest);
   const created=await command(taskId,{kind:'createChat',spaceId:projectId,cwd:project,config:await config(provider),title:`Work QA ${provider}`});assert.equal(created.status,'applied');
   if(provider==='claude-code'){
    await call('SaveProjectCommands',{projectId,expectedActions:[],actions:[{id:'bounded-service',name:'Bounded QA service',command:'sleep 90',waitForSetup:false}]});
    manifest.service=await call('RunProjectCommand',{chatId:taskId,actionId:'bounded-service',requestId:crypto.randomUUID()});save('manifest',manifest);save('service-before',manifest.service);assert.equal(manifest.service.status,'running');
   }
   const marker=`WORK_QA_${crypto.randomBytes(5).toString('hex')}`;
   const prompt=provider==='claude-code'
    ? `Native background-work QA. Use your built-in Agent tool exactly once, with run_in_background=true. Its only task is to run Bash command "sleep 35" once, then reply ${marker}. Do not use any LUNA tools or create_thread. After launching the native agent, immediately reply "Launched" without waiting for it. Do not edit files, use the network, or create other agents.`
    : `Native background-work QA. Use your built-in spawn_agent tool exactly once (possibly in the collaboration namespace). Its only task is to run the shell command "sleep 20" once, then reply ${marker}. Do not use any LUNA tools or create_thread. After launching the native agent, immediately reply "Launched" without waiting for it. Do not execute the sleep in the parent. If spawn_agent is unavailable, reply "NATIVE_AGENT_UNAVAILABLE" and do nothing else. Do not edit files, use the network, or create other agents.`;
   const messageId=crypto.randomUUID();manifest.messages[provider]=messageId;save('manifest',manifest);save(`${provider}-attempt`,{taskId,messageId,prompt});
   await call('QueueMessage',{chatId:taskId,messageId,text:prompt,holdForTurnEnd:true});
   const running=await until(async()=>{const value=await work(taskId);return value.items.some(i=>i.kind==='nativeChild'&&!['completed','failed','stopped','unknown'].includes(i.state))&&value},`${provider} native child`);save(`${provider}-running`,running);
   const children=running.items.filter(i=>i.kind==='nativeChild');assert.equal(children.length,1);assert.equal(children[0].owned,true);assert.equal(children[0].blocksCompletion,true);
   if(provider==='codex'){assert.equal(children[0].canStop,false);save('codex-stop-unavailable',{detail:children[0].detail});await until(async()=>{const value=await work(taskId);return value.items.some(i=>i.id===children[0].id&&['completed','failed','stopped'].includes(i.state))&&value},'Codex child finished',90000)}
   else{
    assert.equal(children[0].canStop,true);const workIds=[children[0].id];const preview=await call('PreviewStopTaskWork',{taskId,workIds});save('claude-stop-preview',preview);
    const args={taskId,workIds,previewId:preview.previewId,requestId:crypto.randomUUID()};manifest.stopArgs=args;save('manifest',manifest);save('claude-stop-attempt',args);
    const receipt=await call('StopTaskWork',args);manifest.stopReceipt=receipt;save('manifest',manifest);save('claude-stop-receipt',receipt);assert.ok(['requested','stopped'].includes(receipt.results[0].status));
    assert.deepEqual(await call('StopTaskWork',args),receipt);
    const services=await call('ProjectCommandStatus',{chatId:taskId});save('service-survived',services);assert.equal(services.runs.find(r=>r.id===manifest.service.id).status,'running');
   }
  }
  const claudeTask=manifest.tasks['claude-code'];
  await until(async()=>{const value=await work(claudeTask);return value.items.filter(i=>i.kind==='nativeChild').every(i=>['completed','failed','stopped'].includes(i.state))},'Claude child terminal before root shell',90000);
  const shellPrompt='Background shell QA: use Bash exactly once with command "sleep 8" and run_in_background=true. Immediately reply "Shell launched". Do not create an agent, edit files, or use the network.';
  manifest.shellMessage=crypto.randomUUID();save('manifest',manifest);save('claude-shell-attempt',{taskId:claudeTask,messageId:manifest.shellMessage,prompt:shellPrompt});
  await call('QueueMessage',{chatId:claudeTask,messageId:manifest.shellMessage,text:shellPrompt,holdForTurnEnd:true});
  const shellRunning=await until(async()=>{const value=await work(claudeTask);return value.items.some(i=>i.kind==='shell'&&i.state==='running')&&value},'Claude native background shell');
  save('claude-shell-running',shellRunning);assert.equal(shellRunning.items.filter(i=>i.kind==='shell'&&i.state==='running').length,1,'launching shell and native job double counted');
  await finish(manifest);
 }else{
  evidence=fs.readFileSync(path.join(base,'latest'),'utf8').trim();assert.ok(evidence.startsWith(`${base}/run-`));const manifest=JSON.parse(fs.readFileSync(path.join(evidence,'manifest.json'),'utf8'));
  if(mode==='inspect'){
   const observation={pid,port,observedAt:new Date().toISOString(),tasks:{}};
   for(const[provider,taskId]of Object.entries(manifest.tasks))observation.tasks[provider]={work:await work(taskId),transcript:await transcript(taskId),identity:await identity(taskId)};
   save(`inspection-${Date.now()}`,observation);console.log(JSON.stringify(observation,null,2));
  }else if(mode==='finish-readback')await finish(manifest);
  else{assert.equal(manifest.completed,true);assert.notEqual(pid,manifest.pid);const snapshots={};for(const [provider,taskId]of Object.entries(manifest.tasks)){const value=await work(taskId);assert.ok(value.items.every(i=>!i.canStop));snapshots[provider]=value}assert.deepEqual(await call('StopTaskWork',manifest.stopArgs),manifest.stopReceipt);save('restart',{pid,port,snapshots});console.log(JSON.stringify({passed:true,evidence,restarted:true,pid}));}
 }
}catch(error){
 if(evidence){
  save(`failure-${mode}`,{message:String(error),time:new Date().toISOString()});
  const manifestPath=path.join(evidence,'manifest.json');
  if(fs.existsSync(manifestPath))for(const[provider,taskId]of Object.entries(JSON.parse(fs.readFileSync(manifestPath,'utf8')).tasks??{})){
   try{save(`${provider}-failure-work`,await work(taskId));save(`${provider}-failure-transcript`,await transcript(taskId))}catch(readError){save(`${provider}-failure-readback`,{message:String(readError)})}
  }
 }
 throw error;
}finally{ws.close()}
