import json,sys
from pathlib import Path
sys.path.insert(0,'test/rustty')
import parity_reference as parity
import hyperlink_lifecycle_requests
s=Path('/private/tmp/rustty-reintegrate.leadij')
parity.ARTIFACTS=s/'linked-native-generation';parity.ARTIFACTS.mkdir(exist_ok=True)
peer=parity.Peer('native',['zig-out/bin/vt-oracle'],15)
try:
 fixtures=[{'request':request,'covers':covers} for request,covers in hyperlink_lifecycle_requests.requests(peer) if request['id'].startswith('pages/hyperlinks/batched/')]
finally:peer.close()
assert len(fixtures)==44,len(fixtures)
(s/'linked-native-fixtures.json').write_text(json.dumps(fixtures,indent=2)+'\n')
print(len(fixtures),'fresh native fixtures')
