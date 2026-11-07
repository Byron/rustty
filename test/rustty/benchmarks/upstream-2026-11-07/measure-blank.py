from pathlib import Path
import subprocess,json,time,hashlib
s=Path('/private/tmp/rustty-reintegrate.leadij')
cases=[('plain',70),('plain-styled-protected',50),('styled-1',20),('styled-4',25),('wide',80),('wide-spaces',20),('mixed-wide',15),('erase-background',2)]
for kind in ['2','22','rate','lines']:
 for dims in ['120x80','120x12','300x100']:
  n={'2':[3,8,1],'22':[8,16,4],'rate':[4,12,2],'lines':[8,16,4]}[kind][['120x80','120x12','300x100'].index(dims)]
  cases.append((f'blank-prefix-erase-{kind}-{dims}',n))
meta={'started':time.time(),'before':'db4827ff87215ff3be8fa892eac3f6ac4c4b484d / movrxsk','compiler':subprocess.check_output(['rustc','+1.95.0','-vV'],text=True),'profile':'release thinLTO codegen-units=1 locked/offline','binaries':{n:hashlib.sha256((s/n).read_bytes()).hexdigest() for n in ['blank-before-stream','blank-after-stream','blank-before-controlled','blank-after-controlled','sgr-native-primitives']}}
results=[]
for round in range(1,4):
 for name,n in cases:
  checks=[]
  for v in (['before','after','native'] if round%2 else ['native','after','before']):
   argv=([str(s/'sgr-native-primitives'),'stream_file'] if v=='native' else [str(s/f'blank-{v}-stream')])+[str(s/'stream-configs'/(name+'.json')),str(n)]
   p=subprocess.run(argv,capture_output=True,text=True);assert p.returncode==0,(argv,p.stderr)
   r=json.loads(p.stdout);checks.append(r['checksum']);results.append({'round':round,'name':name,'version':v,'argv':argv,'result':r,'stderr':p.stderr,'status':p.returncode})
   (s/'blank-shared-timings.json').write_text(json.dumps(results,indent=2)+'\n')
  assert len(set(checks))==1,(name,checks)
  print(round,name,'complete',flush=True)
controlled=[]
# Each driver reports three internal rounds; swap process order for alternating cases.
for i,case in enumerate(['plain','erase-2','erase-2-seen','erase-22','erase-lines']):
 n=20000 if case=='plain' else 400
 for v in (['before','after'] if i%2 else ['after','before']):
  argv=[str(s/f'blank-{v}-controlled'),case,str(n),'80'];p=subprocess.run(argv,capture_output=True,text=True);assert p.returncode==0,(argv,p.stderr)
  controlled.append({'name':case,'version':v,'argv':argv,'results':[json.loads(line) for line in p.stdout.splitlines()],'status':p.returncode,'stderr':p.stderr})
  (s/'blank-controlled-timings.json').write_text(json.dumps(controlled,indent=2)+'\n')
 print('controlled',case,'complete',flush=True)
meta['finished']=time.time();(s/'blank-measurement-run.json').write_text(json.dumps(meta,indent=2)+'\n')
