#!/usr/bin/env python3
"""Publish/sync extra co-op UI assets from an already imported disposable SME setup."""
import argparse,json,subprocess,time
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
for n in ('binary','work','host','base','downloads'):p.add_argument('--'+n,type=Path,required=True)
a=p.parse_args();assert (a.work/'.editable-acceptance.json').is_file()
previous=sorted(a.work.glob('assets-evidence-*'))
e=a.work/('assets-evidence-'+str(time.time_ns()));e.mkdir()
friend=a.work/'friend';before={str(p.relative_to(friend/'instance')):(p.stat().st_ino,p.stat().st_mtime_ns) for p in (friend/'instance').rglob('*') if p.is_file() and '.skyrimsync-install' not in p.name}
measurements=[]
def cli(label,*args):
 t=time.monotonic()
 with (e/(label+'.json')).open('wb') as out,(e/(label+'.progress.jsonl')).open('wb') as err:
  r=subprocess.run([str(a.binary),'--json',*map(str,args)],stdout=out,stderr=err)
 assert r.returncode==0,(label,e)
 v=json.loads((e/(label+'.json')).read_text());measurements.append({'step':label,'seconds':round(time.monotonic()-t,3)})
 print(json.dumps(measurements[-1]),flush=True);return v
reuse=None
for old in reversed(previous):
 if (old/'publish.json').is_file() and (old/'publish.json').stat().st_size:
  candidate=json.loads((old/'publish.json').read_text())
  if candidate.get('revision')==json.loads((a.host/'channel-head.json').read_text())['revision']:
   reuse=old;break
if reuse:
 import shutil
 first=json.loads((reuse/'publish.json').read_text());shutil.copyfile(reuse/'complete-coop.skyrimsync',e/'complete-coop.skyrimsync')
 measurements.append({'step':'publish','reused':str(reuse)})
else:first=cli('publish','publish','--coop',a.host,'--output',e/'complete-coop.skyrimsync')
original=json.loads(sorted(a.work.glob('evidence-*/summary.json'))[-1].read_text())
assert first['code']==original['code']
with (e/'server.log').open('wb') as log:
 server=subprocess.Popen([str(a.binary),'--json','serve','--coop',str(a.host),'--no-relay','--bind','127.0.0.1:0'],stdout=subprocess.PIPE,stderr=log,text=True)
 try:
  ready=json.loads(server.stdout.readline());addr=next(x for v in ready['addresses']['addrs'] if isinstance(v,dict) for k,x in v.items() if k.lower()=='ip')
  result=cli('sync','sync',first['code'],'--peer-address',addr,'--coop',friend,'--base',a.base,'--downloads',a.downloads)
  assert result['files_replaced']==8 and result['staged_bytes']==182444,result
  for rel,identity in before.items():
   f=friend/'instance'/rel;assert (f.stat().st_ino,f.stat().st_mtime_ns)==identity,rel
  cli('friend-verify','verify',e/'complete-coop.skyrimsync','--instance',friend/'instance')
  summary={'state':'passed','result':result,'package':str(e/'complete-coop.skyrimsync'),'unchanged_files':len(before),'code_unchanged':True,'measurements':measurements,'runtime_verified':False}
  (e/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({'state':'coop_assets_passed','evidence':str(e)}),flush=True)
 finally:
  server.terminate()
  try:server.wait(timeout=5)
  except subprocess.TimeoutExpired:server.kill();server.wait()
