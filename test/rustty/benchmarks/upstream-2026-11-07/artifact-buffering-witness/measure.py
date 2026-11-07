"""Time the existing Rust comparison runner; verify saved bytes outside timing."""
import datetime,hashlib,json,pathlib,shutil,statistics,subprocess,time
S=pathlib.Path('/private/tmp/rustty-reintegrate.leadij');D=S/'artifact-buffering-witness';ROOT=pathlib.Path('/Users/byron/dev/github.com/ghostty-org/ghostty');expected=json.loads((D/'expected.json').read_text());by_id={x['id']:x for x in expected['expected_by_id']}
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
runners={'before':S/'final-parity/binaries/runner','after':ROOT/'target/release/examples/parity-runner'}
oracles={'zig':S/'final-parity-corrected-group2/binaries/zig','rust':S/'final-parity-corrected-group2/binaries/rust'}
assert sha(oracles['zig'])=='ffed07e73c1a44e3cae1cec05308a47aeb60bf7e990d763faba96ddca1234ccc'
assert sha(oracles['rust'])=='e575c280123c53f46caed59c7469484feae59b81ab5d87982cb7ef671da1e1b5'
assert sha(runners['before'])=='0a2dbe6ec2d5a01aad642a689d49727ea9a699279a2fe0b54d64c67db5e14de7'
identities={k:{'path':str(v),'sha256':sha(v)} for k,v in {**runners,**oracles}.items()};shutil.copy2(runners['after'],D/'buffered-runner')
plan=[('warmup',0,'before'),('warmup',0,'after')]+[('timed',i,v) for i,order in enumerate([['before','after'],['after','before'],['before','after']],1) for v in order]
metadata={'scope':'Whole Rust-runner wall time including process/oracle startup, comparisons and saved failure artifacts; excludes post-run artifact hashing. Not terminal throughput or UI latency.','source':'Buffering-only uncommitted runner candidate onca7e1373; root validated13tests/Clippy/fmt before handoff.','runner_comparison_engine':'Existing Rust parity-runner; Python schedules processes and verifies recorded file hashes only.','identities':identities,'frozen_after':str(D/'buffered-runner'),'fixture_sha256':sha(D/'fixtures.json'),'expected_manifest_sha256':sha(D/'expected.json'),'driver_sha256':sha(pathlib.Path(__file__)),'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'runs':[]}
(D/'measurement.json').write_text(json.dumps(metadata,indent=2)+'\n')
for phase,round_,version in plan:
 label=f'{phase}-{round_}-{version}';artifacts=D/label;log=D/(label+'.log');assert not artifacts.exists()
 argv=[str(runners[version]),'--no-build','--rust-bin',str(oracles['rust']),'--zig-bin',str(oracles['zig']),'--fixtures',str(D/'fixtures.json'),'--artifacts',str(artifacts),'--max-failures','100']
 started=time.perf_counter_ns()
 with log.open('wb') as output:result=subprocess.run(argv,cwd=ROOT,stdout=output,stderr=subprocess.STDOUT)
 elapsed=time.perf_counter_ns()-started
 summary=json.loads((artifacts/'summary.json').read_text());assert result.returncode==1 and summary['checked']==6 and summary['failures']==6 and not summary['aborted'] and summary.get('error') is None
 files=[];seen=set()
 for p in sorted((artifacts/'failures').iterdir()):
  q=json.loads((p/'request.json').read_text());id=q['id'];assert id in by_id and id not in seen;seen.add(id)
  for name,record in by_id[id]['files'].items():
   f=p/name;digest=sha(f);assert f.stat().st_size==record['bytes'] and digest==record['sha256'],(label,id,name,digest)
   files.append({'id':id,'file':name,'sha256':digest,'bytes':f.stat().st_size})
 assert seen==set(by_id)
 metadata['runs'].append({'phase':phase,'round':round_,'version':version,'argv':argv,'log':str(log),'artifacts':str(artifacts),'exit_status':result.returncode,'elapsed_ns':elapsed,'summary':summary,'summary_sha256':sha(artifacts/'summary.json'),'all24_artifact_files_exact':True,'files':files})
 (D/'measurement.json').write_text(json.dumps(metadata,indent=2)+'\n');print(label,round(elapsed/1e9,6),'s;24files exact',flush=True)
for k,v in {**runners,**oracles}.items():assert sha(v)==identities[k]['sha256']
assert len({x['summary_sha256'] for x in metadata['runs']})==1
pairs=[]
for i in [1,2,3]:
 samples={x['version']:x['elapsed_ns'] for x in metadata['runs'] if x['phase']=='timed' and x['round']==i};pairs.append({'round':i,**samples,'after_vs_before_percent':100*(samples['after']/samples['before']-1)})
medians={v:statistics.median(x['elapsed_ns'] for x in metadata['runs'] if x['phase']=='timed' and x['version']==v) for v in runners}
metadata.update({'finished_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'completed':True,'pairs':pairs,'median_elapsed_ns':medians,'median_after_vs_before_percent':100*(medians['after']/medians['before']-1),'observations':'Bounded two-fixture batch only; do not extrapolate to terminal throughput or all workload sizes.','all192_saved_files_exact':True})
(D/'measurement.json').write_text(json.dumps(metadata,indent=2)+'\n');print(json.dumps({'pairs':pairs,'medians_ns':medians,'median_percent':metadata['median_after_vs_before_percent'],'all192_saved_files_exact':True},indent=2),flush=True)
