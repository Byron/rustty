from pathlib import Path
import subprocess,json,hashlib,time
s=Path('/private/tmp/rustty-reintegrate.leadij');results=[]
names=[f'erase-{kind}-{dims}' for kind in ['2','22','rate','lines'] for dims in ['120x80','120x12','300x100']]+['plain','plain-styled-protected','erase-background','styled-1','styled-4','wide','wide-spaces','mixed-wide']
for name in names:
 row={'name':name,'results':{}}
 for version in ['before','after','native']:
  argv=([str(s/'sgr-native-primitives'),'stream_file'] if version=='native' else [str(s/f'blank-{version}-stream')])+[str(s/'stream-configs'/(name+'.json')),'1']
  p=subprocess.run(argv,capture_output=True,text=True);assert p.returncode==0,(name,version,p.stderr)
  row['results'][version]=json.loads(p.stdout)
 assert len({x['checksum'] for x in row['results'].values()})==1,row
 results.append(row);(s/'blank-input-selfchecks.json').write_text(json.dumps(results,indent=2)+'\n')
 print(name,'PASS',flush=True)
