#!/usr/bin/env python3
"""Exercise changed-file repair on existing disposable real acceptance instances.
Never modifies the host/base, copies a game tree or launches Skyrim. Run under a
process-tree memory cap when exercising the installer. Keeps per-command evidence.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
import zipfile

p=argparse.ArgumentParser(description=__doc__)
for name in ('binary','package','work','reconstructed','base','downloads'):
    p.add_argument('--'+name, type=Path, required=True)
p.add_argument("--sources-only", action="store_true", help="Exercise real inline INI generation, an archive patch and reuse of the previous DLL repair output.")
p.add_argument("--resume-sources", action="store_true", help="Resume sources-only acceptance after a failed repair left its three deliberate deletions intact.")
a=p.parse_args()
assert not a.resume_sources or a.sources_only
work=a.work.resolve()
assert (work/'.acceptance-driver.json').is_file(), 'Expected disposable real acceptance folder.'
friend=work/'friend/instance'; host=work/'host'
assert json.loads((work/'friend/.skyrimsync-owner.json').read_text())['kind']=='skyrim-mod-sync-storage'
with zipfile.ZipFile(a.package) as z:
    manifest=json.loads(z.read('manifest.json'))
files=manifest['files']; target_files={f['path']: f for f in files}
evidence=work/('incremental-evidence-'+str(time.time_ns()));evidence.mkdir()
measurements=[]
def cli(label,*args,code=0):
    start=time.monotonic()
    metrics=evidence/(label+'.metrics')
    with (evidence/(label+'.json')).open('wb') as out,(evidence/(label+'.progress.jsonl')).open('wb') as err:
        r=subprocess.run(['/usr/bin/time','-f','max_rss_kib=%M\nelapsed_seconds=%e','-o',str(metrics),str(a.binary.resolve()),'--json',*map(str,args)],stdout=out,stderr=err)
    measurements.append({'step':label,'exit':r.returncode,'seconds':round(time.monotonic()-start,3),'metrics':metrics.read_text()})
    print(json.dumps(measurements[-1]),flush=True)
    assert r.returncode==code, f'{label}: inspect {evidence}'
    return json.loads((evidence/(label+'.json')).read_text())
def locate(root,rel):
    for part in rel.split('/'):
        found=[p for p in root.iterdir() if p.name.casefold()==part.casefold()]
        assert len(found)==1,(root,part)
        root=found[0]
    return root
def identity(path):
    stat=path.stat()
    return stat.st_ino,stat.st_size,stat.st_mtime_ns
common=['--base',a.base.resolve(),'--coop',work/'friend','--downloads',a.downloads.resolve()]
assert cli('host_before','verify',a.package,'--instance',host)['files_match']
source_changes={'config/documents/Skyrim.ini', next(n for n in target_files if n.lower().endswith('/crashlogger.ini')), next(n for n in target_files if n.lower().endswith('/po3_tweaks.dll'))}
comparison=cli('friend_before','verify',a.package,'--instance',friend,code=1 if a.resume_sources else 0)
assert comparison['files_match']==(not a.resume_sources)
if a.resume_sources:
    assert {d.get('path') for d in comparison['differences']}==source_changes
before={f['path']:identity(locate(friend,f['path'])) for f in files if not (a.resume_sources and f['path'] in source_changes)}
if a.sources_only:
    changed={'config/documents/Skyrim.ini', next(n for n in target_files if n.lower().endswith('/crashlogger.ini')), next(n for n in target_files if n.lower().endswith('/po3_tweaks.dll'))}
    if not a.resume_sources:
        for n in changed: locate(friend,n).unlink()
    plan=cli('sources_plan','plan',a.package,*common)
    assert plan['files_replaced']==3
    assert len(plan['acquisition']['verified_cached'])==1
    assert plan['acquisition']['missing_bytes']==0
    result=cli('sources_repair','sync',a.package,*common)
    assert result['files_replaced']==3
    assert result['staged_bytes']==sum(target_files[n]['size'] for n in changed)
    progress=[json.loads(l) for l in (evidence/'sources_repair.progress.jsonl').read_text().splitlines() if l.startswith('{')]
    assert any(e['state']=='repair_acquisition' and e['detail']=={'outputs':1,'archives':1} for e in progress),progress
    for n,old in before.items():
        if n not in changed: assert identity(locate(friend,n))==old,n
    assert cli('friend_final','verify',a.package,'--instance',friend)['files_match']
    assert cli('host_final','verify',a.package,'--instance',host)['files_match']
    summary={'state':'passed','runtime_verified':False,'changed':sorted(changed),'inline_ini_generated':True,'archive_patch_verified':True,'previous_dll_output_reused':True,'repair':result,'measurements':measurements}
    (evidence/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps({'state':'incremental_sources_acceptance_passed','summary':str(evidence/'summary.json')}),flush=True)
    raise SystemExit()
dll=next(n for n in target_files if n.lower().endswith('/po3_tweaks.dll'))
script=next(n for n in target_files if n.lower().endswith('/actorutil.pex'))
ini='config/documents/Skyrim.ini';order='config/local/loadorder.txt'
changed={dll,script,ini,order}
locate(friend,dll).unlink()
f=locate(friend,ini);f.write_bytes(f.read_bytes()+b'\n[ModSyncIncrementalAcceptance]\nbChanged=1\n')
f=locate(friend,order);lines=f.read_text(encoding='utf-8-sig').splitlines()
active={p['name'].lower() for p in manifest['snapshot']['plugins']}
indices=[i for i,n in enumerate(lines) if n.lower() in active]
lines[indices[-2]],lines[indices[-1]]=lines[indices[-1]],lines[indices[-2]]
f.write_text('\n'.join(lines)+'\n')
f=locate(friend,script);stat=f.stat();data=bytearray(f.read_bytes());data[-1]^=1;f.write_bytes(data);os.utime(f,ns=(stat.st_atime_ns,stat.st_mtime_ns))
cli('friend_changed','verify',a.package,'--instance',friend,code=1)
plan=cli('warm_plan','plan',a.package,'--reconstructed',a.reconstructed.resolve(),*common)
assert plan['staged_bytes']==sum(target_files[n]['size'] for n in changed)
assert plan['acquisition']['missing_bytes']==0 and plan['acquisition']['reconstruction_bytes']==0
repair=cli('warm_repair','sync',a.package,'--reconstructed',a.reconstructed.resolve(),*common)
assert repair['files_replaced']==4 and repair['files_removed']==0
assert repair['staged_bytes']==plan['staged_bytes']
for n,old in before.items():
    if n not in changed: assert identity(locate(friend,n))==old,n
backup=Path(repair['backup']); contents=[p.relative_to(backup).as_posix() for p in backup.rglob('*') if p.is_file()]
assert set(contents)=={script,ini,order,'.skyrimsync-install.json','.skyrimsync-delta.json'},contents
assert cli('warm_verify','verify',a.package,'--instance',friend)['files_match']
cli('rollback','rollback','--coop',work/'friend')
cli('rollback_verify','verify',a.package,'--instance',friend,code=1)
for n,old in before.items():
    if n not in changed: assert identity(locate(friend,n))==old,n
cli('repair_after_rollback','sync',a.package,'--reconstructed',a.reconstructed.resolve(),*common)
# No full reconstruction in this storage: one absent DLL must use a subset recipe.
assert not (work/'friend/jobs'/manifest['artifact']['sha256']/'reconstructed').exists()
locate(friend,dll).unlink()
# Remove this output from earlier targeted jobs to force an archive recovery again.
source=target_files[dll]['source']['path']
for job in (work/'friend/jobs').glob('*/reconstructed'):
    try: locate(job,source).unlink()
    except (AssertionError,FileNotFoundError): pass
cold_plan=cli('cold_plan','plan',a.package,*common)
assert cold_plan['files_replaced']==1
assert len(cold_plan['acquisition']['verified_cached'])==1
assert cold_plan['acquisition']['missing_bytes']==0
cold=cli('cold_repair','sync',a.package,*common)
assert cold['files_replaced']==1 and cold['staged_bytes']==target_files[dll]['size']
progress=[json.loads(l) for l in (evidence/'cold_repair.progress.jsonl').read_text().splitlines() if l.startswith('{')]
assert any(e['state']=='repair_acquisition' and e['detail']=={'outputs':1,'archives':1} for e in progress),progress
# Every engine job should contain only its requested DLL, plus engine-owned metadata.
jobs=list((work/'friend/jobs').glob('*/reconstructed'))
assert jobs
for job in jobs:
    runtime=[p.relative_to(job).as_posix() for p in job.rglob('*') if p.is_file() and p.suffix.lower()=='.dll']
    assert len(runtime)==1,runtime
assert cli('friend_final','verify',a.package,'--instance',friend)['files_match']
assert cli('host_final','verify',a.package,'--instance',host)['files_match']
for n,old in before.items():
    if n not in changed: assert identity(locate(friend,n))==old,n
summary={'state':'passed','runtime_verified':False,'target_files':len(files),'unchanged_files_preserved':len(files)-len(changed),'warm_repair':repair,'cold_repair':cold,'backup_files':contents,'measurements':measurements}
(evidence/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps({'state':'incremental_acceptance_passed','summary':str(evidence/'summary.json')}),flush=True)
