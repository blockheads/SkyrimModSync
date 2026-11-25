#!/usr/bin/env python3
"""Native CLI acceptance for a read-only SME import and frozen peer revisions.
Copies only authoring mods/profile into disposable storage. A licensed base and
original reconstructed game remain read-only. Never launches a game in this driver.
"""
import argparse,json,os,shutil,subprocess,time
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
for n in ('binary','raw','recipe','base','downloads','work','host'):
    p.add_argument('--'+n,type=Path,required=True)
p.add_argument('--resume',action='store_true')
a=p.parse_args();work=a.work.resolve();work.mkdir(parents=True,exist_ok=a.resume)
marker=work/'.editable-acceptance.json'
if a.resume: assert marker.is_file()
else: marker.write_text(json.dumps({'kind':'disposable-editable-SME','raw':str(a.raw.resolve())}))
previous=sorted(work.glob('evidence-*'))
evidence=work/('evidence-'+str(time.time_ns()));evidence.mkdir();measurements=[]
def cli(label,*args,code=0):
    started=time.monotonic();metrics=evidence/(label+'.metrics')
    with (evidence/(label+'.json')).open('wb') as out,(evidence/(label+'.progress.jsonl')).open('wb') as err:
        r=subprocess.run(['/usr/bin/time','-f','max_rss_kib=%M\nelapsed_seconds=%e','-o',str(metrics),str(a.binary.resolve()),'--json',*map(str,args)],stdout=out,stderr=err)
    measurements.append({'step':label,'exit':r.returncode,'seconds':round(time.monotonic()-started,3),'metrics':metrics.read_text()})
    print(json.dumps(measurements[-1]),flush=True)
    assert r.returncode==code, f'{label}: inspect {evidence}'
    return json.loads((evidence/(label+'.json')).read_text())
authoring=work/'authoring'
profile='Skyrim Modding Essentials'
if not a.resume:
    shutil.copytree(a.raw/'mods',authoring/'mods',symlinks=False)
    shutil.copytree(a.raw/'profiles'/profile,authoring/'profiles'/profile,symlinks=False)
    print(json.dumps({'step':'copied_authoring','state':'done'}),flush=True)
local=authoring/'mods/Co-op Local'
loader=a.base/'Data/SKSE/Plugins/SkyrimCoopLoader.dll';client=a.base/'Data/SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll'
for src,rel in [(loader,'SKSE/Plugins/SkyrimCoopLoader.dll'),(client,'SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll')]:
    dest=local/rel;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dest)
listfile=authoring/'profiles'/profile/'modlist.txt'
raw=listfile.read_text(encoding='utf-8-sig')
if '+Co-op Local' not in raw: listfile.write_text('+Co-op Local\n'+raw)
paths=['--base',a.base.resolve(),'--downloads',a.downloads.resolve()]
hostpaths=[*paths,'--coop',a.host.resolve()]
reuse=None
if a.resume and not (local/'readme-modsync.txt').exists():
    for old in reversed(previous):
        published=old/'publish_one.json'
        if published.is_file() and published.stat().st_size and (old/'one.skyrimsync').is_file():
            candidate=json.loads(published.read_text())
            head=json.loads((a.host/'channel-head.json').read_text())
            if candidate.get('revision')==head['revision']:
                reuse=old;break
if reuse:
    first=json.loads((reuse/'publish_one.json').read_text())
    shutil.copyfile(reuse/'one.skyrimsync',evidence/'one.skyrimsync')
    for label in ('import','publish_one'):
        measurements.append({'step':label,'reused_verified_step':str(reuse),'exit':0})
    print(json.dumps({'step':'resume_published_revision','revision':first['revision']}),flush=True)
else:
    cli('import','import','--mo2',authoring,'--game',a.raw/'Stock Game','--profile',profile,'--recipe',a.recipe.resolve(),*hostpaths)
    first=cli('publish_one','publish','--output',evidence/'one.skyrimsync',*hostpaths)
server_log=(evidence/'server.progress.jsonl').open('wb')
server=subprocess.Popen([str(a.binary.resolve()),'--json','serve','--coop',str(a.host.resolve()),'--no-relay','--bind','127.0.0.1:0'],stdout=subprocess.PIPE,stderr=server_log,text=True)
try:
    ready=json.loads(server.stdout.readline())
    address=next(v for v in ready['addresses']['addrs'] if 'ip' in str(v).lower())
    # EndpointAddr serialization may represent IP transports as a tagged object.
    def ip_address(v):
        if isinstance(v,str): return v
        if isinstance(v,dict):
            for key,x in v.items():
                if key.lower()=='ip': return x
        raise AssertionError(v)
    addr=ip_address(address)
    friend=work/'friend'
    common=[*paths,'--coop',friend,'--peer-address',addr]
    one=cli('sync_one','sync',first['code'],*common)
    # Prove bytes are peer-supplied even though the host's personal base has them.
    installed=json.loads((friend/'installed-revision.json').read_text())
    coop=next(e for e in installed['files'] if e['path'].lower().endswith('/skyrimcoop/skyrimtogetherclient.dll'))
    assert coop['source']['kind']=='blob',coop
    assert (friend/'instance'/coop['path']).stat().st_size==client.stat().st_size
    original={e['path']:(friend/'instance'/e['path']).stat().st_ino for e in installed['files']}
    # Add/change/remove an ordinary mod asset, then use the very same code.
    asset=local/'readme-modsync.txt';asset.write_bytes(b'local revision two')
    second=cli('publish_two','publish','--output',evidence/'two.skyrimsync',*hostpaths)
    assert second['code']==first['code']
    two=cli('sync_two','sync',first['code'],*common)
    assert two['files_replaced']==1 and two['staged_bytes']==len(b'local revision two'),two
    for rel,ino in original.items(): assert (friend/'instance'/rel).stat().st_ino==ino,rel
    asset.write_bytes(b'local revision three')
    third=cli('publish_three','publish','--output',evidence/'three.skyrimsync',*hostpaths)
    three=cli('sync_three','sync',first['code'],*common)
    assert third['code']==first['code'] and three['files_replaced']==1
    asset.unlink()
    fourth=cli('publish_four','publish','--output',evidence/'four.skyrimsync',*hostpaths)
    four=cli('sync_four','sync',first['code'],*common)
    assert fourth['code']==first['code'] and four['files_replaced']==0 and four['files_removed']==1
    cli('friend_verify','verify',evidence/'four.skyrimsync','--instance',friend/'instance')
    cli('host_verify','verify',evidence/'four.skyrimsync','--instance',a.host/'instance')
    summary={'state':'passed','code':first['code'],'host':str(a.host),'friend':str(friend),'authoring':str(authoring),'runtime_verified':False,'sequence':fourth['sequence'],'measurements':measurements}
    (evidence/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps({'state':'editable_acceptance_passed','summary':str(evidence/'summary.json')}),flush=True)
finally:
    server.terminate()
    try: server.wait(timeout=5)
    except subprocess.TimeoutExpired: server.kill();server.wait()
    server_log.close()
