import json,subprocess,time,uuid,sys,os,threading,tempfile
work=tempfile.mkdtemp(prefix='race-cwd-')
argv=['claude','--print','--input-format','stream-json','--output-format','stream-json','--verbose','--include-partial-messages',
      '--replay-user-messages','--permission-prompt-tool','stdio','--permission-mode','bypassPermissions','--dangerously-skip-permissions',
      '--model','claude-haiku-5-5']
env={k:v for k,v in os.environ.items() if not k.startswith('CLAUDECODE') and k!='CLAUDE_CODE_ENTRYPOINT'}
t0=time.time()
p=subprocess.Popen(argv,cwd=work,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,bufsize=1)
out=[]
def reader():
    for line in p.stdout:
        out.append((round(time.time()-t0,3), line.rstrip('\n')))
th=threading.Thread(target=reader,daemon=True); th.start()
def send(text, delay):
    time.sleep(delay)
    u=str(uuid.uuid4())
    p.stdin.write(json.dumps({'type':'user','uuid':u,'message':{'role':'user','content':[{'type':'text','text':text}]},'parent_tool_use_id':None,'session_id':''})+'\n'); p.stdin.flush()
    out.append((round(time.time()-t0,3), json.dumps({'_sent':text,'uuid':u})))
send('Reply with exactly RACE-A. Do not use tools.',0)
send('Reply with exactly RACE-B. Do not use tools.',0.3)
deadline=time.time()+90
while time.time()<deadline:
    results=[1 for _,l in out if '"type":"result"' in l]
    if len(results)>=2: break
    time.sleep(0.5)
time.sleep(1.5); p.stdin.close(); p.terminate(); p.wait(timeout=10)
json.dump([{'t':t,'line':l} for t,l in out], open(sys.argv[1],'w'), indent=0)
print('frames',len(out),'results',sum(1 for _,l in out if '"type":"result"' in l))
