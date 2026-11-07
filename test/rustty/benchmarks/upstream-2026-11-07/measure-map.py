from pathlib import Path
import subprocess,json,time,hashlib
s=Path('/private/tmp/rustty-reintegrate.leadij')
cases=[('plain',100),('styled-4',20),('links-full-120x80-0',4),('links-full-215x215-0',3),('links-short-120x80-10000',4),('links-short-120x80-50000000',3),('links-short-215x215-10000',3)]
meta={'before':'06ddbdb5a / ztmmomo','started':time.time(),'profile':'release,thinLTO,codegen-units=1,locked offline','compiler':subprocess.check_output(['rustc','+1.95.0','-vV'],text=True),'binaries':{p:hashlib.sha256((s/p).read_bytes()).hexdigest()for p in ['map-before-stream','map-after-stream','sgr-native-primitives']}}
results=[]
for round in range(1,4):
 for name,iterations in cases:
  checks=[]
  for version in (['before','after','native']if round%2 else ['native','after','before']):
   config=str(s/'stream-configs'/f'{name}.json')
   argv=([str(s/'sgr-native-primitives'),'stream_file',config,str(iterations)]if version=='native'else[str(s/f'map-{version}-stream'),config,str(iterations)])
   p=subprocess.run(argv,capture_output=True,text=True);assert p.returncode==0,(name,version,p.stderr);r=json.loads(p.stdout)
   if name.endswith('-0'):assert r['history_rows']==0,(name,version,r['history_rows'])
   checks.append(r['checksum']);results.append({'round':round,'name':name,'version':version,'argv':argv,'result':r,'status':p.returncode,'stderr':p.stderr})
   (s/'map-shared-timings.json').write_text(json.dumps(results,indent=2)+'\n')
  assert len(set(checks))==1,(name,checks)
  print(round,name,'complete',flush=True)
meta['finished']=time.time();(s/'map-measurement-run.json').write_text(json.dumps(meta,indent=2)+'\n')
