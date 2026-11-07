from pathlib import Path
import subprocess,json,time,hashlib,sys
s=Path('/private/tmp/rustty-reintegrate.leadij')
cases=[('wide',60),('wide-unicode',40),('combining-control',20),('emoji-control',20),('wide-spaces',15),('mixed-wide',10)]
bins={'pre-blank':s/'blank-before-stream','before':s/'mixed-before-stream','after':s/'mixed-after-stream','native':Path('zig-out/bin/vt-primitives').resolve()}
mode=sys.argv[1] if len(sys.argv)>1 else 'measure'
output=s/('mixed-selfchecks.json' if mode=='check' else 'mixed-timings.json')
meta={'started':time.time(),'baseline':'595623207b12eafe0d178e15ac26d67a1aabf472 / krlvrvl','compiler':subprocess.check_output(['rustc','+1.95.0','-vV'],text=True),'profile':'release thinLTO codegen-units=1 locked/offline','binaries':{n:{'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()} for n,p in bins.items()},'configs':{name:json.loads((s/'stream-configs'/(name+'.json')).read_text()) for name,_ in cases}}
results=[]
for round in range(1,2 if mode=='check' else 4):
 for name,iterations in cases:
  versions=['pre-blank','before','after','native'] if name.startswith(('wide','mixed-wide')) else ['before','after','native']
  if round%2==0:versions.reverse()
  checks=[];histories=[]
  for version in versions:
   argv=[str(bins[version])]+(['stream_file'] if version=='native' else [])+[str(s/'stream-configs'/(name+'.json')),str(1 if mode=='check' else iterations)]
   process=subprocess.run(argv,capture_output=True,text=True)
   assert process.returncode==0,(argv,process.stderr)
   result=json.loads(process.stdout);checks.append(result['checksum']);histories.append(result['history_rows'])
   results.append({'round':round,'name':name,'version':version,'argv':argv,'result':result,'stderr':process.stderr,'status':process.returncode})
   output.write_text(json.dumps(results,indent=2)+'\n')
  assert len(set(checks))==1,(name,checks)
  assert len(set(histories))==1,(name,histories)
  print(round,name,'complete',flush=True)
meta['finished']=time.time();(s/('mixed-selfcheck-run.json' if mode=='check' else 'mixed-measurement-run.json')).write_text(json.dumps(meta,indent=2)+'\n')
