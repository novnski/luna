// SW-12 read-only native readback of the original LUN-1/2/11/12/13 records in
// the primary LUNA Dev. No write method is allowlisted.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';

const root='/Users/iulian/workspace/code/personal/luna';
const prior=path.join(root,'target/qa/board-automation-fixes-2026-10-10');
const out=path.join(root,'target/qa/sw-12/native');
const bundle=path.join(root,'target/luna-macos-dev/LUNA Dev.app');
const executable=fs.realpathSync(path.join(bundle,'Contents/MacOS/luna'));
const data=path.join(root,'target/luna-macos-dev/data');
const source=JSON.parse(fs.readFileSync(path.join(prior,'selected-context.json')));
const priorReport=JSON.parse(fs.readFileSync(path.join(prior,'native-verification-latest.json')));
const priorLun12=JSON.parse(fs.readFileSync(path.join(priorReport.evidence,'lun12-after.json')));
const ticket='wi-38b3b86b4532773e6752328bdf37fb66';
const expectedSha=priorReport.originalImageSha256;
const ids=[...new Set(execFileSync('lsof',['-nP','-iTCP:59777','-sTCP:LISTEN','-Fp'],{encoding:'utf8'}).match(/^p\d+$/gm))];
assert.equal(ids.length,1);
const pid=ids[0].slice(1);
assert.equal(execFileSync('ps',['-p',pid,'-o','comm='],{encoding:'utf8'}).trim(),executable);
execFileSync('codesign',['--verify','--deep','--strict',bundle],{stdio:'pipe'});
const files=execFileSync('lsof',['-p',pid,'-Fn'],{encoding:'utf8'});
assert.ok(files.includes('n'+path.join(data,'profiles/local/docs.sqlite3')+'\n'),'primary local profile must be the open database');
fs.mkdirSync(out,{recursive:true});
const save=(name,value)=>fs.writeFileSync(path.join(out,name+'.json'),JSON.stringify(value,null,2)+'\n');
const allowed=new Set(['EngineInfo','BoardList','BoardRead','BoardChatBadges','ReadAttachmentChunk']);
const ws=new WebSocket('ws://127.0.0.1:59777');
await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject;});
let seq=0;
async function call(method,params={}) {
  assert.ok(allowed.has(method),method+' is not a read');
  return new Promise((resolve,reject)=>{
    const id=++seq;
    const timer=setTimeout(()=>{ws.removeEventListener('message',receive);ws.send(JSON.stringify({id,cancel:true}));reject(Error(method+' timed out'));},20000);
    function receive(e) {
      for(const line of String(e.data).split('\n').filter(Boolean)) {
        const f=JSON.parse(line);
        if(f.id!==id||!['ok','err','item'].some(k=>Object.hasOwn(f,k)))continue;
        clearTimeout(timer);ws.removeEventListener('message',receive);
        if(Object.hasOwn(f,'item'))ws.send(JSON.stringify({id,cancel:true}));
        f.err?reject(Error(JSON.stringify(f.err))):resolve(Object.hasOwn(f,'item')?f.item:f.ok);
      }
    }
    ws.addEventListener('message',receive);ws.send(JSON.stringify({id,method,params}));
  });
}
try {
  const info=await call('EngineInfo');
  assert.equal(info.deviceId,priorReport.deviceId,'same primary device');
  const lun12=await call('BoardRead',{workItemId:ticket,projectId:source.chat.spaceId,historyLimit:100});
  save('lun12',lun12);
  const item=lun12.view.item, was=priorLun12.view.item;
  assert.equal(item.key,'LUN-12');
  for(const field of ['id','key','projectId','ownerHostId','title','description','archived','deleted','attachments']) {
    assert.deepEqual(item[field],was[field],field+' must be preserved since 10 October');
  }
  const canonical=item.attachments.find(p=>p.endsWith('8ebd13c3-image.png'));
  assert.ok(canonical,'the original screenshot stays attached');
  const hash=crypto.createHash('sha256');
  let offset=0;
  for(let n=0;n<1000;n++) {
    const chunk=await call('ReadAttachmentChunk',{path:canonical,offset});
    hash.update(Buffer.from(chunk.data,'base64'));
    if(chunk.done)break;
    assert.ok(chunk.nextOffset>offset);offset=chunk.nextOffset;
  }
  const imageSha=hash.digest('hex');
  assert.equal(imageSha,expectedSha,'image bytes read back over IPC');
  const scope={kind:'project',projectId:source.chat.spaceId};
  const active=await call('BoardList',{scope,limit:100});
  const archived=await call('BoardList',{scope,archived:'only',limit:100});
  const badges=await call('BoardChatBadges',{taskIds:[source.chat.id]});
  save('active-tickets',active);save('archived-tickets',archived);save('source-chat-badges',badges);
  const activeKeys=active.items.map(row=>row.item.key), archivedKeys=archived.items.map(row=>row.item.key);
  const visible=(badges.related??[]).filter(row=>!row.archived).map(row=>row.key);
  for(const key of priorReport.activeKeys) assert.ok(activeKeys.includes(key),key+' still active');
  for(const key of priorReport.archivedKeys) assert.ok(archivedKeys.includes(key),key+' still archived');
  for(const key of priorReport.visibleRelatedKeys) assert.ok(visible.includes(key),key+' still related to the source chat');
  const events=lun12.recentEvents.map(e=>`${e.seq}:${e.id}:r${e.revision}`);
  assert.deepEqual(events,priorLun12.recentEvents.map(e=>`${e.seq}:${e.id}:r${e.revision}`),'audit events since 10 October preserved');
  assert.deepEqual(lun12.links,priorLun12.links,'source chat link preserved');
  const report={at:new Date().toISOString(),pid,executable,signature:'verified',deviceId:info.deviceId,writesAllowlisted:false,lun12Revision:item.revision,priorRevision:was.revision,auditEvents:events,sourceLinks:lun12.links.map(l=>`${l.role}:${l.taskId}:${l.active}`),imageSha256:imageSha,activeKeys,archivedKeys,visibleRelatedKeys:visible};
  save('verification',report);
  console.log(JSON.stringify(report,null,2));
} finally { ws.close(); }
