"""Artifact-only full-response comparison; no oracle subprocesses or classifier changes."""
import copy,hashlib,json,pathlib,re,subprocess
S=pathlib.Path('/private/tmp/rustty-reintegrate.leadij'); ROOT=pathlib.Path.cwd()
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def mapping(d):return {json.loads(p.read_text())['id']:p.parent for p in d.glob('*/request.json')}
def diff(a,b,p='response'):
 if type(a)!=type(b):return [{'path':p,'native':a,'rust':b}]
 if isinstance(a,dict):
  if a.keys()!=b.keys():return [{'path':p,'native_keys':list(a),'rust_keys':list(b)}]
  return sum((diff(a[k],b[k],p+'.'+k) for k in a),[])
 if isinstance(a,list):
  if len(a)!=len(b):return [{'path':p,'native':a,'rust':b}]
  return sum((diff(x,y,p+f'[{i}]') for i,(x,y) in enumerate(zip(a,b))),[])
 return [] if a==b else [{'path':p,'native':a,'rust':b}]
final=mapping(S/'final-parity/groups-3/failures'); before=mapping(S/'final-stream-gap-baseline/failures'); horizontal=mapping(S/'final-stream-cell-horizontal-baseline/failures');rows=[]
for id,p in sorted(final.items()):
 if not id.startswith('corpus/'):continue
 z=json.loads((p/'zig.json').read_text());r=json.loads((p/'rust.json').read_text()); b=before[id]
 assert json.loads((b/'rust.json').read_text())==r
 assert json.loads((b/'zig.json').read_text())==z
 native=copy.deepcopy(z);rust=copy.deepcopy(r); native.pop('capabilities',None);rust.pop('capabilities',None)
 paths=diff(native,rust)
 category='older_osc66_extension' if id in horizontal else 'older_decxcpr_unsupported'
 exact = {'frozen_pre_link_rust_complete_reply_equals_final':True,'repeated_native_complete_reply_equals_final':True}
 if id in horizontal:
  h=horizontal[id];assert json.loads((h/'rust.json').read_text())==r;assert json.loads((h/'zig.json').read_text())==z
  exact['frozen_horizontal_rust_complete_reply_equals_final']=True
  assert all(d['path'].startswith('response.observations') for d in paths)
  evidence={'horizontal_artifact':str(h),'source_identity':'final-parser-stream-source-identity.json','rust_origin':'e0f2ab23cfab7aae2dc0bc6a1873e4375dc85b44','native_origin':'916b99df7','reason':'OSC66 default-width text printing is an older Rust extension. Native stream deliberately leaves kitty_text_sizing unimplemented; Rust prints accepted payload text, including semicolon fragments in these corpus sequences.'}
 else:
  removed=[]; i=j=0;ze=native['events'];re_=rust['events']
  while i<len(ze):
   if j<len(re_) and ze[i]==re_[j]:i+=1;j+=1;continue
   e=ze[i];data=bytes.fromhex(e['data'])
   assert e['kind']=='write' and re.fullmatch(rb'(?:\x1b\[\?\d+;\d+;1R)+',data)
   removed.append({'native_index':i,'event':e});i+=1
  assert j==len(re_)
  native['events']=[e for i,e in enumerate(ze) if i not in {x['native_index'] for x in removed}]
  assert native==rust
  exact['all_fields_equal_after_only_extra_DECXCPR_writes_removed']=True
  evidence={'native_origin':'fbda61b873da5208475c183af160d131ac7a77a3','reason':'Native supports CSI ? 6 n extended cursor report; existing Rust csi handles ordinary (false,6) only. Neither handler changed in incoming range.','extra_DECXCPR_events':removed}
 rows.append({'id':id,'category':category,'artifact':str(p),'baseline_artifact':str(b),'hashes':{f:sha(p/f) for f in ['request.json','zig.json','rust.json','difference.txt']},'baseline_hashes':{f:sha(b/f) for f in ['request.json','zig.json','rust.json','difference.txt']},'proof':exact,'differences_after_standard_root_capabilities_exclusion':paths,'evidence':evidence})
assert len(rows)==21
bins=[ROOT/'target/release/examples/parity-runner',ROOT/'zig-out/bin/vt-oracle',S/'linked-before-parity',S/'horizontal-rust-oracle']
result={'analysis':'Complete raw reply equality across recorded runs; root capabilities is the sole standard runner exclusion for native/Rust field-delta listing. No fixture or classifier changed.','case_count':21,'counts':{'older_decxcpr_unsupported':12,'older_osc66_extension':9},'all_baseline_complete_replies_equal':True,'binaries':{str(p):sha(p) for p in bins},'baseline_provenance':{'linked-before-parity':{'revision':'1aa7813cdb93cf59cc9ac869654d705a9e1291db','source':'mixed-completed.json records mixed-after-parity; linked-before-parity is byte-identical','note':'Pre-link, not pre-task.'},'horizontal-rust-oracle':{'revision':'b57ffe1f13ab5d0c0d031a2f530f940f44c66683','source':'Root session freeze for horizontal600-comparison matrix; function identity also verified at originalc06197283 and this revision','note':'After first horizontal behavior port, before remaining behavior/performance ports; not a pre-task binary.'}},'source_identity':'final-parser-stream-source-identity.json','commands':[{'argv':['target/release/examples/parity-runner','--no-build','--rust-bin',str(S/'linked-before-parity'),'--zig-bin','zig-out/bin/vt-oracle','--fixtures',str(S/'final-stream-gap-baseline-fixtures.json'),'--artifacts',str(S/'final-stream-gap-baseline'),'--max-failures','100'],'log':str(S/'final-stream-gap-baseline.log'),'exit_code':1,'summary':json.loads((S/'final-stream-gap-baseline/summary.json').read_text())},{'argv':['target/release/examples/parity-runner','--no-build','--rust-bin',str(S/'horizontal-rust-oracle'),'--zig-bin','zig-out/bin/vt-oracle','--fixtures',str(S/'final-stream-cell-baseline-fixtures.json'),'--artifacts',str(S/'final-stream-cell-horizontal-baseline'),'--max-failures','100'],'log':str(S/'final-stream-cell-horizontal-baseline.log'),'exit_code':1,'summary':json.loads((S/'final-stream-cell-horizontal-baseline/summary.json').read_text())}],'cases':rows}
(S/'final-stream-gap-attribution.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'case_count':len(rows),'all_complete_baseline_replies_equal':True,'field_deltas':sum(len(x['differences_after_standard_root_capabilities_exclusion']) for x in rows),'per_case_deltas':{x['id']:len(x['differences_after_standard_root_capabilities_exclusion']) for x in rows}},indent=2))
