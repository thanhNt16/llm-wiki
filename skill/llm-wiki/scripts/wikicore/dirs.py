import json, os, tempfile, time
from . import ids, sources
from .transaction import TxnError
SKIP_DIRS={'.git','.svn','.hg','node_modules','__pycache__','.venv','venv','dist','build','target','vendor','.idea','.llm-wiki'}
DEFAULT_MAX_BYTES=50*1024*1024
def _now(): return time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime())
def _classify_skip(path,rel,name,is_dir,include_hidden,max_bytes):
 if is_dir:
  if name in SKIP_DIRS:return 'vcs/vendor dir'
  if name.startswith('.') and not include_hidden:return 'hidden'
  return None
 if name in sources.SKIP_NAMES or os.path.splitext(name)[1].lower() in sources.SKIP_EXTS:return 'non-evidence binary'
 if name.startswith('.') and not include_hidden:return 'hidden'
 if os.path.islink(path):return 'symlink'
 try:
  if os.path.getsize(path)>max_bytes:return 'oversize'
 except OSError:return 'unreadable'
 return None
def _walk(root,include_hidden,max_bytes):
 out=[]; skipped={}
 for dp,dns,fns in os.walk(root,followlinks=False):
  dns.sort();fns.sort(); dr=os.path.relpath(dp,root)
  inherited=next((reason for anc,reason in skipped.items() if dr==anc or dr.startswith(anc+os.sep)),None)
  inherited=next((reason for anc,reason in skipped.items() if dr==anc or dr.startswith(anc+os.sep)),None)
  if dr == '.llm-wiki' or dr.startswith('.llm-wiki'+os.sep): inherited='__omit__'
   p=os.path.join(dp,d);rel=os.path.relpath(p,root);reason=inherited or _classify_skip(p,rel,d,True,include_hidden,max_bytes)
   if reason:
    if d != '.llm-wiki': out.append((rel,'skipped',reason))
    skipped[rel]=reason
  for f in fns:
   p=os.path.join(dp,f);rel=os.path.relpath(p,root);reason=inherited or _classify_skip(p,rel,f,False,include_hidden,max_bytes);out.append((rel,'skipped',reason) if reason else (rel,'file',None))
 return sorted(out)
def ingest_dir(wiki,path,*,include_hidden=False,max_bytes=DEFAULT_MAX_BYTES,force=False):
 if not wiki.exists():raise TxnError('wiki not initialized; run wiki-init first')
 if not os.path.isdir(path):raise TxnError('not a directory: %s'%path)
 root=os.path.abspath(path);items=[];counts={'ingested':0,'deduplicated':0,'skipped':0,'errors':0}
 for rel,act,reason in _walk(root,include_hidden,max_bytes):
  if act=='skipped':items.append({'path':rel,'status':'skipped','reason':reason});counts['skipped']+=1;continue
  try:
   with open(os.path.join(root,rel),'rb') as f:data=f.read()
   ref=os.path.join(root,rel)
   if os.path.basename(ref).startswith('.'):
    with tempfile.NamedTemporaryFile(suffix=os.path.splitext(ref)[1]) as t:t.write(data);t.flush();r=sources.ingest(wiki,'file',t.name,data,force=force,root_origin='dir:'+root)
   else:r=sources.ingest(wiki,'file',ref,data,force=force,root_origin='dir:'+root)
  except Exception as e:items.append({'path':rel,'status':'error','error':str(e)});counts['errors']+=1;continue
  status='deduplicated' if r.get('deduplicated') else 'ingested';counts[status]+=1;items.append({'path':rel,'status':status,'source_id':r['source_id'],'version':r['version']})
 receipt={'batch_id':ids.new('batch'),'dir':root,'captured_at':_now(),'counts':counts,'items':items};od=wiki.p('state','ingest-batches');os.makedirs(od,exist_ok=True)
 with open(os.path.join(od,receipt['batch_id']+'.json'),'w') as f:json.dump(receipt,f,indent=2);f.write('\n')
 return receipt
