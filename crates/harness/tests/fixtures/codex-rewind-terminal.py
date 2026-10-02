#!/usr/bin/env python3
"""Bounded native app-server process fixture: terminal history + exact revert."""
import json,os,sys
from pathlib import Path
root=Path(os.environ['CODEX_HOME']);root.mkdir(parents=True,exist_ok=True)
def turns(thread):
 status=thread.removeprefix('rewind-')
 if status=='missing':status=None
 first={'id':'t1','items':[{'id':'i1','type':'agentMessage','text':'retained'}]}
 if status is not None:first['status']='inProgress' if status=='active' else status
 second={'id':'t1' if status=='duplicate' else 't2','status':'completed','items':[{'id':'i2','type':'agentMessage','text':'removed'}]}
 if status=='duplicate':first['status']='completed'
 if status=='failed':first['error']={'message':'historical failure'}
 return [first] if (root/('reverted-'+thread)).exists() else [first,second]
for line in sys.stdin:
 j=json.loads(line);method=j.get('method');params=j.get('params',{});thread=params.get('threadId','');ident=j.get('id')
 if ident is None:continue
 if method=='initialize':result={'userAgent':'rewind-terminal-fixture'}
 elif method in ('thread/read','thread/resume'):
  result={'thread':{'id':thread,'historyMode':'paginated','ephemeral':False,'status':{'type':'idle'},'cwd':str(root)}}
 elif method=='thread/turns/list':result={'data':turns(thread),'nextCursor':None,'backwardsCursor':None}
 elif method=='thread/revert':
  assert params.get('beforeTurnId')=='t2'
  (root/('reverted-'+thread)).write_text('once')
  result={'thread':{'id':thread}}
 else:raise RuntimeError(method)
 print(json.dumps({'id':ident,'result':result}),flush=True)
