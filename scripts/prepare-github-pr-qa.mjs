#!/usr/bin/env node
// Creates only disposable local fixtures. --seed writes only to QA IPC 59790.
import { mkdirSync, writeFileSync, existsSync, realpathSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { execFileSync } from 'node:child_process';
const root = resolve('target/qa/github-pr-native');
const checkout = join(root, 'checkout');
const fixture = realpathSync('scripts/fixtures/github-pr');
mkdirSync(checkout, {recursive: true});
const git = (...args) => execFileSync('git', args, {cwd:checkout, encoding:'utf8', stdio:['ignore','pipe','pipe']}).trim();
if (!existsSync(join(checkout,'.git'))) {
  git('init','-q','-b','main');
  writeFileSync(join(checkout,'README.md'), '# Draft pull request fixture\n');
  git('add','README.md');
  git('-c','user.name=QA','-c','user.email=qa@example.invalid','commit','-qm','Initial fixture');
  git('checkout','-qb','feature/pr');
  writeFileSync(join(checkout,'README.md'), '# Draft pull request fixture\n\nA change ready for review.\n');
  git('add','README.md');
  git('-c','user.name=QA','-c','user.email=qa@example.invalid','commit','-qm','Document draft pull request workflow');
  git('remote','add','origin','git@github.com:acme/luna.git');
  git('update-ref','refs/remotes/origin/feature/pr',git('rev-parse','HEAD'));
  git('update-ref','refs/remotes/origin/main',git('rev-parse','main'));
  git('symbolic-ref','refs/remotes/origin/HEAD','refs/remotes/origin/main');
  git('config','branch.feature/pr.remote','origin');
  git('config','branch.feature/pr.merge','refs/heads/feature/pr');
}
writeFileSync(join(root,'sha'),git('rev-parse','HEAD'));
if (!existsSync(join(root,'mode'))) writeFileSync(join(root,'mode'),'success');
writeFileSync(join(root,'launch-env.json'),JSON.stringify({LUNA_PR_FIXTURE:root, LUNA_NO_LOGIN_SHELL:'1', PATH:`${fixture}:${process.env.PATH}`},null,2));
if (!process.argv.includes('--seed')) {
  console.log(JSON.stringify({root,checkout,launchEnvironment:join(root,'launch-env.json')}));
  process.exit(0);
}
const ws = new WebSocket('ws://127.0.0.1:59790');
await new Promise((resolve,reject)=>{ws.addEventListener('open',resolve,{once:true}); ws.addEventListener('error',reject,{once:true});});
let sequence=0;
async function call(method,params={}) {
 const id=++sequence;
 return await new Promise((resolve,reject)=>{
  const timer=setTimeout(()=>reject(new Error(`${method} timeout`)),12000);
  const receive=event=>{const frame=JSON.parse(event.data); if(frame.id!==id)return;clearTimeout(timer); ws.removeEventListener('message',receive); frame.err?reject(new Error(JSON.stringify(frame.err))):resolve(frame.ok);};
  ws.addEventListener('message',receive);ws.send(JSON.stringify({id,method,params}));
 });
}
try {
 const info=await call('EngineInfo');
 const spaceId='github-pr-qa-space', chatId='github-pr-qa-chat';
 await call('Mutate',{op:'createSpace',spaceId,deviceId:info.deviceId,path:checkout,name:'Pull Request QA',gitDetected:true});
 await call('Mutate',{op:'createChat',chatId,spaceId,deviceId:info.deviceId,cwd:checkout,branch:'feature/pr',config:{harness:'codex',model:'gpt-6-luna',sandbox:'workspace-write'}});
 await call('Mutate',{op:'renameChat',chatId,title:'Draft pull request acceptance'});
 console.log(JSON.stringify({spaceId,chatId,checkout,deviceId:info.deviceId}));
} finally {ws.close();}
