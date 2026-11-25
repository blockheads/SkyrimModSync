#!/usr/bin/env python3
"""Optional integration driver: executes the native CLI, never launches Skyrim.

Requires a real SME deployed tree, its original recipe/reconstruction, and a licensed
base. Creates fresh disposable game copies. Python is only the test driver, not a CLI
runtime dependency. A fresh work directory prevents accidental modification of an
existing install. Each CLI result, progress log and memory measurement is retained.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
for name in ('binary','source','recipe','reconstructed','base','downloads','work'):
    p.add_argument('--'+name, type=Path, required=True)
p.add_argument('--resume', action='store_true')
a = p.parse_args()
work = a.work.absolute()
work.mkdir(parents=True, exist_ok=a.resume)
assert not work.is_relative_to(a.source.resolve())
assert not a.source.resolve().is_relative_to(work)
evidence=work/('evidence-retry-'+str(time.time_ns()) if a.resume else 'evidence')
evidence.mkdir()
measurements=[]
def cli(label,*args,code=0):
    started=time.monotonic()
    resultfile=evidence/(label+'.json')
    metrics=evidence/(label+'.metrics')
    with resultfile.open('wb') as out,(evidence/(label+'.progress.jsonl')).open('wb') as err:
        result=subprocess.run(['/usr/bin/time','-f','max_rss_kib=%M\nelapsed_seconds=%e','-o',str(metrics),str(a.binary.resolve()),'--json',*map(str,args)],stdout=out,stderr=err)
    measurements.append({'step':label,'exit':result.returncode,'seconds':round(time.monotonic()-started,3),'metrics':metrics.read_text()})
    print(json.dumps(measurements[-1]),flush=True)
    if result.returncode!=code:
        raise RuntimeError(f'{label}: expected exit {code}, got {result.returncode}; inspect {resultfile}')
    return json.loads(resultfile.read_text())
def capture(label,root):
    output=evidence/(label+'.snapshot.json')
    result=cli(label,'capture','--game',root/'game','--documents',root/'config/documents','--local',root/'config/local','--output',output)
    return output,result
common=['--base',a.base.resolve(),'--coop',work/'friend','--downloads',a.downloads.resolve()]
host=work/'host'
started=time.monotonic()
if not a.resume:
    for name in ('game','config'):
        shutil.copytree(a.source/name,host/name,symlinks=False)
else:
    assert (work/'.acceptance-driver.json').is_file(), 'Refuse resume without driver ownership marker.'
    assert json.loads((work/'.acceptance-driver.json').read_text())['source']==str(a.source.resolve())
(work/'.acceptance-driver.json').write_text(json.dumps({'source':str(a.source.resolve()),'kind':'disposable-real-sync-test'}))
print(json.dumps({'step':'copy_host','seconds':round(time.monotonic()-started,3)}),flush=True)
assert (a.source/'game/SkyrimSE.exe').stat().st_ino!=(host/'game/SkyrimSE.exe').stat().st_ino
hostsnap,captured=capture('host',host)
assert not captured['warnings'],captured['warnings']
package=work/('host-'+str(time.time_ns())+'.skyrimsync' if a.resume else 'host.skyrimsync')
cli('publish','publish','--snapshot',hostsnap,'--recipe',a.recipe.resolve(),'--profile','Skyrim Modding Essentials','--reconstructed',a.reconstructed.resolve(),'--output',package,*common)
plan=cli('plan','plan',package,*common)
assert not plan['blockers'],plan['blockers']
cli('sync_initial','sync',package,'--reconstructed',a.reconstructed.resolve(),*common)
friend=work/'friend/instance'
def friend_file(rel):
    current=friend
    for part in rel.split('/'):
        found=[p for p in current.iterdir() if p.name.casefold()==part.casefold()]
        assert len(found)==1,(rel,part)
        current=found[0]
    return current

assert (host/'game/SkyrimSE.exe').stat().st_ino!=(friend/'game/SkyrimSE.exe').stat().st_ino
friendsnap,_=capture('friend_initial',friend)
assert cli('compare_initial','compare',hostsnap,friendsnap)['files_match']
# Deliberate real-world differences: a deleted native mod, shared setting change,
# plugin reordering and same-size/same-mtime loose script edit.
snap=json.loads(hostsnap.read_text())
dll=next(f['path'] for f in snap['files'] if f['path'].lower().endswith('/po3_tweaks.dll'))
friend_file(dll).unlink()
ini=friend/'config/documents/Skyrim.ini'
ini.write_bytes(ini.read_bytes()+b'\n[ModSyncAcceptance]\nbChanged=1\n')
order=friend/'config/local/loadorder.txt'
raw=order.read_bytes();lines=raw.decode('utf-8-sig').splitlines()
active={p['name'].lower() for p in snap['plugins']}
indices=[i for i,n in enumerate(lines) if n.lower() in active]
lines[indices[-2]],lines[indices[-1]]=lines[indices[-1]],lines[indices[-2]]
order.write_text('\n'.join(lines)+'\n')
script=next(f['path'] for f in snap['files'] if f['path'].lower().endswith('.pex'))
file=friend_file(script)
before=file.stat();data=bytearray(file.read_bytes());data[-1]^=1;file.write_bytes(data);os.utime(file,ns=(before.st_atime_ns,before.st_mtime_ns))
changed_snap,_=capture('friend_changed',friend)
differences=cli('compare_changed','compare',hostsnap,changed_snap,code=1)
kinds={d['kind'] for d in differences['differences']}
assert {'missing','changed','plugin_order'}<=kinds,differences
assert any(d.get('path')==script and d['kind']=='changed' for d in differences['differences'])
assert any(d.get('path')==dll and d['kind']=='missing' for d in differences['differences'])
repair=cli('sync_repair','sync',package,'--reconstructed',a.reconstructed.resolve(),*common)
assert repair['backup_retained']
fixed_snap,_=capture('friend_repaired',friend)
assert cli('compare_repaired','compare',hostsnap,fixed_snap)['files_match']
assert cli('verify_repaired','verify',package,'--instance',friend)['files_match']
# Prove a manually added co-op DLL cannot be published by the SME recipe.
manual=host/'game/Data/SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll'
manual.parent.mkdir(parents=True,exist_ok=True)
base_dll=a.base/'Data/SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll'
assert base_dll.is_file(), 'Real co-op build needed for the added-binary provenance check.'
shutil.copyfile(base_dll,manual)
manual_snap,_=capture('host_untracked_coop',host)
refusal=cli('publish_untracked_coop','publish','--snapshot',manual_snap,'--recipe',a.recipe.resolve(),'--profile','Skyrim Modding Essentials','--reconstructed',a.reconstructed.resolve(),'--output',work/'refused.skyrimsync',*common,code=2)
assert refusal['reason']=='unreproducible_target',refusal
assert not (work/'refused.skyrimsync').exists()
manual.unlink()
# Re-read host after all steps: the friend repair never changed it.
finalhost,_=capture('host_final',host)
assert cli('compare_host_unchanged','compare',hostsnap,finalhost)['files_match']
summary={'state':'passed','real_files':captured['files'],'target_id':captured['target_id'],'versions':captured['versions'],'plugins':captured['plugins'],'deliberate_differences':differences['differences'],'runtime_verified':False,'measurements':measurements,'source':str(a.source),'host':str(host),'friend':str(friend),'physical_copies':True}
(evidence/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps({'state':'acceptance_passed','summary':str(evidence/'summary.json')}),flush=True)
