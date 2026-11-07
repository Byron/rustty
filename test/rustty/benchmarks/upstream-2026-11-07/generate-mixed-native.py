import json,sys
from pathlib import Path
sys.path.insert(0,'test/rustty')
import parity_reference as parity
import grapheme_lifecycle_requests
s=Path('/private/tmp/rustty-reintegrate.leadij')
parity.ARTIFACTS=s/'mixed-native-generation';parity.ARTIFACTS.mkdir(exist_ok=True)
peer=parity.Peer('native',['zig-out/bin/vt-oracle'],15)
try:
 fixtures=[{'request':request,'covers':covers} for request,covers in grapheme_lifecycle_requests.requests(peer) if request['id'].startswith('pages/graphemes/mixed-batch/')]
finally:peer.close()
assert len(fixtures)==54,len(fixtures)
(s/'mixed-native-fixtures.json').write_text(json.dumps(fixtures,indent=2)+'\n')
print(len(fixtures),'fresh native fixtures')
