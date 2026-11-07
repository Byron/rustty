import subprocess, pathlib, json, shutil, hashlib, time
s=pathlib.Path('/private/tmp/rustty-reintegrate.leadij')
argv=['--bench','rustty/resource_churn','--noplot','--sample-size','20','--warm-up-time','1','--measurement-time','2']
metadata={'started':time.time(),'compiler':subprocess.check_output(['rustc','+1.95.0','-vV'],text=True),'argv':argv,'binaries':{},'runs':[]}
for tag in ['before','after']:
 metadata['binaries'][tag]=hashlib.sha256((s/f'style-{tag}-bench').read_bytes()).hexdigest()
for n in range(1,4):
 for tag in (['before','after'] if n%2 else ['after','before']):
  log=s/f'style-{tag}-{n}.log'
  with log.open('w') as f:r=subprocess.run([str(s/f'style-{tag}-bench'),*argv],stdout=f,stderr=subprocess.STDOUT)
  metadata['runs'].append({'round':n,'version':tag,'status':r.returncode,'log':log.name})
  assert r.returncode==0
  dest=s/f'style-samples-{tag}-{n}';dest.mkdir(exist_ok=True)
  for kind in ['plain','style','hyperlink']:
   source=pathlib.Path('target/criterion/rustty_resource_churn')/kind/'new'
   if source.exists():shutil.copytree(source,dest/kind,dirs_exist_ok=True)
  print(f'{tag} {n} complete',flush=True)
metadata['finished']=time.time();(s/'style-measurement-run.json').write_text(json.dumps(metadata,indent=2)+'\n')
