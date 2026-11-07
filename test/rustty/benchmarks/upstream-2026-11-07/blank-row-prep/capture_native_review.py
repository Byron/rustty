from pathlib import Path
import hashlib,json,sys
sys.path.insert(0,'test/rustty')
import parity_reference as parity
root=Path('/private/tmp/rustty-reintegrate.leadij/blank-row-prep')
parity.ARTIFACTS=root/'native-review-raw'
parity.ARTIFACTS.mkdir(exist_ok=True)
binaries={'rust':'/private/tmp/rustty-reintegrate.leadij/blank-after-parity',
          'zig':'zig-out/bin/vt-oracle'}
peers={name:parity.Peer(name,[path],15) for name,path in binaries.items()}
failures=[]
count=0
try:
 with (parity.ARTIFACTS/'responses.jsonl').open('w') as output:
  for filename in ['native-review-fixtures.json','native-review-kitty.json']:
   for fixture in json.loads((root/filename).read_text()):
    for request in parity.variants(fixture['request']):
     left,right,diff=parity.compare([peers['zig'],peers['rust']],request)
     responses={'zig':left,'rust':right}
     output.write(json.dumps({'request':request,**responses,'difference':diff})+'\n')
     count+=1
     if diff: failures.append({'id':request['id'],'difference':diff})
finally:
 for peer in peers.values(): peer.close()
summary={'comparisons':count,'failures':failures,
         'binaries':{path:hashlib.sha256(Path(path).read_bytes()).hexdigest() for path in binaries.values()},
         'command':['python3',str(root/'capture_native_review.py')],
         'note':'Existing parity_reference Peer/variants/difference APIs; no new repository harness. Raw observations saved for successful cases too.'}
(root/'native-review-validation.json').write_text(json.dumps(summary,indent=2)+'\n')
print(f'{count} captured comparisons; {len(failures)} failures')
if failures: sys.exit(1)
