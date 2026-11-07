from pathlib import Path
import subprocess,json,time,hashlib
s=Path('/private/tmp/rustty-reintegrate.leadij')
cases=[('plain',70),('styled-1',20),('styled-4',20),('links-full-120x80-0',3),('links-full-215x215-0',2),('links-short-120x80-10000',3),('links-short-120x80-50000000',3),('links-short-215x215-10000',3),('links-full-120x80-10000',1),('links-full-120x80-50000000',1),('links-full-215x215-10000',1),('links-full-215x215-50000000',1)]
meta={'before':'3882d7da6 / nokoswn','premap':'06ddbdb5a / ztmmomo (measured source; current equivalent ddfc413a6)','started':time.time(),'profile':'release,thinLTO,codegen-units=1,locked offline','compiler':subprocess.check_output(['rustc','+1.95.0','-vV'],text=True),'binaries':{n:hashlib.sha256((s/n).read_bytes()).hexdigest()for n in ['carry-before-stream','carry-after-stream','map-before-stream','sgr-native-primitives']}}
results=[]
for round in range(1,4):
 for name,iterations in cases:
  versions=['before','after','native']
  if not(name.startswith('links-full-') and not name.endswith('-0')):versions.insert(2,'premap')
  if round%2==0:versions.reverse()
  checks=[]
  for v in versions:
   c=str(s/'stream-configs'/f'{name}.json')
   argv=([str(s/'sgr-native-primitives'),'stream_file',c,str(iterations)]if v=='native'else[str(s/('map-before-stream' if v=='premap'else f'carry-{v}-stream')),c,str(iterations)])
   p=subprocess.run(argv,capture_output=True,text=True);assert p.returncode==0,(name,v,p.stderr);r=json.loads(p.stdout)
   if name.endswith('-0'):assert r['history_rows']==0,(name,v,r['history_rows'])
   checks.append(r['checksum']);results.append({'round':round,'name':name,'version':v,'argv':argv,'result':r,'status':p.returncode,'stderr':p.stderr})
   (s/'carry-shared-timings.json').write_text(json.dumps(results,indent=2)+'\n')
  assert len(set(checks))==1,(name,checks)
  print(round,name,'complete',flush=True)
meta['finished']=time.time();(s/'carry-measurement-run.json').write_text(json.dumps(meta,indent=2)+'\n')
