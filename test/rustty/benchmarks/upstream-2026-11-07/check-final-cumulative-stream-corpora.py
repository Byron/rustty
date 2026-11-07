from pathlib import Path
import hashlib,json,re,subprocess,time
s=Path('/private/tmp/rustty-reintegrate.leadij')
manifest=json.loads((s/'upstream-corpora/manifest.json').read_text())
assert len(manifest['corpora'])==22
binaries={'rust':Path('target/release/examples/stream-file').resolve(),'zig':Path('zig-out/bin/vt-primitives').resolve()}
meta={'started':time.time(),'binaries':{engine:{'path':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()} for engine,path in binaries.items()},'command':'python3 '+__file__}
results=[]
for corpus in manifest['corpora']:
 path=s/'upstream-corpora'/corpus['file']
 assert hashlib.sha256(path.read_bytes()).hexdigest()==corpus['sha256']
 name=path.stem; dimensions=re.search(r'-(\d+)x(\d+)$',name)
 cols,rows=map(int,dimensions.groups()) if dimensions else (120,80)
 config={'name':name,'data':str(path),'cols':cols,'rows':rows,'scrollback_bytes':10000,'grapheme_width':'legacy'}
 cfg=s/'stream-configs'/('cumulative-'+name+'.json');cfg.write_text(json.dumps(config)+'\n')
 checks=[];histories=[]
 for engine,binary in binaries.items():
  argv=[str(binary)]+(['stream_file'] if engine=='zig' else [])+[str(cfg),'1']
  process=subprocess.run(argv,capture_output=True,text=True)
  assert process.returncode==0,(argv,process.stderr)
  result=json.loads(process.stdout)
  assert result['units_per_iteration']==corpus['checks']['bytes']
  checks.append(result['checksum']);histories.append(result['history_rows'])
  results.append({'name':name,'engine':engine,'config':config,'argv':argv,'result':result,'stderr':process.stderr,'sha256':corpus['sha256']})
  (s/'final-cumulative-stream-corpora-selfchecks.json').write_text(json.dumps(results,indent=2)+'\n')
 assert len(set(checks))==len(set(histories))==1,(name,checks,histories)
 print(name,'matches',flush=True)
meta['finished']=time.time();meta['corpora']=len(manifest['corpora']);meta['checks']=len(results)
(s/'final-cumulative-stream-corpora-selfcheck-run.json').write_text(json.dumps(meta,indent=2)+'\n')
