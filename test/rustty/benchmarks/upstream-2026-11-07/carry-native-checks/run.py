from pathlib import Path
import gzip,hashlib,json,subprocess,sys
root=Path('/Users/byron/dev/github.com/ghostty-org/ghostty')
scratch=Path(__file__).parent
sys.dont_write_bytecode=True
sys.path.insert(0,str(root/'test/rustty'))
import parity_reference as pr
pr.ARTIFACTS=scratch
peers={name:pr.Peer(name,[str(root/exe)],30) for name,exe in [('rust','target/release/examples/parity'),('zig','zig-out/bin/vt-oracle')]}

def differences(a,b,path='',out=None):
 out=[] if out is None else out
 if len(out)>=24:return out
 if type(a)!=type(b):out.append({'path':path,'rust':a,'zig':b});return out
 if isinstance(a,dict):
  for k in sorted(a.keys()|b.keys()):
   if k not in a or k not in b:out.append({'path':path+'/'+k,'rust':a.get(k),'zig':b.get(k)})
   else:differences(a[k],b[k],path+'/'+k,out)
 elif isinstance(a,list):
  if len(a)!=len(b):out.append({'path':path+'/length','rust':len(a),'zig':len(b)})
  for i,(x,y) in enumerate(zip(a,b)):differences(x,y,path+'/'+str(i),out)
 elif a!=b:out.append({'path':path,'rust':a,'zig':b})
 return out

def data(kind,n,cols,offset=0):
 out=bytearray()
 for y in range(offset,offset+n):
  if kind=='plain':out+=b'plain\r\n';continue
  if kind=='links':
   out+=f'\x1b]8;;https://example.test/{y}\x1b\\'.encode()+b'linkedxx'+b'\x1b]8;;\x1b\\\r\n';continue
  count=300 if kind=='styles' else 12
  for x in range(4):
   v=(y*4+x)%count
   out+=f'\x1b[38;2;{v%256};{v//256};1m'.encode()+bytes([65+x])*(cols//4)
  out+=b'\x1b[0m\r\n'
 return bytes(out)

results=[]; requests=[]; capabilities={}
try:
 caps={}
 for cols in [80,215]:
  q={'id':'carry/capacity/'+str(cols),'kind':'page_layout','page_layout':{'columns':cols}}
  values={name:p.request(q) for name,p in peers.items()}
  capabilities={n:v['capabilities'] for n,v in values.items()}
  assert set(capabilities['rust'])==set(capabilities['zig'])
  assert values['rust']['page_layout']==values['zig']['page_layout'],differences(values['rust']['page_layout'],values['zig']['page_layout'])
  caps[cols]=values['zig']['page_layout']['layout']['capacity']['rows']
  standard_bytes=values['zig']['page_layout']['constants']['standard_bytes']
  results.append({'id':q['id'],'equal':True,'capacity':values['zig']['page_layout']['layout']['capacity']})
 cases=[('styles-steady',80,24,None,['styles','styles']),('links-steady',80,24,None,['links','links']),('styles-burst',80,24,None,['styles','light','plain','plain','plain']),('links-burst',80,24,None,['links','plain','plain','plain']),('tall-style-floor',215,215,0,['styles','plain','plain','plain']),('link-byte-budget',80,24,standard_bytes*3,['links','links','plain','plain','plain'])]
 for name,cols,rows,limit,phases in cases:
  if limit is None:limit=standard_bytes*3
  ops=[{'op':'grid','grid':{'action':'limits','bytes':limit,'lines':None}},{'op':'pages'}]
  offset=0;phase_info=[]
  for phase in phases:
   n=caps[cols]*2 if phase in ['styles','links'] else caps[cols]+1
   stream=data(phase,n,cols,offset);offset+=n
   ops.extend([{'op':'write','data':stream.hex()},{'op':'pages'}])
   phase_info.append({'phase':phase,'lines':n,'bytes':len(stream),'sha256':hashlib.sha256(stream).hexdigest()})
  ops.append({'op':'observe'})
  q={'id':'carry/'+name,'kind':'input','cols':cols,'rows':rows,'operations':ops}
  values={n:p.request(q) for n,p in peers.items()}
  for n,value in values.items():
   with gzip.open(scratch/(name+'-'+n+'.json.gz'),'wt') as f:json.dump(value,f,separators=(',',':'))
  delta=differences({k:v for k,v in values['rust'].items() if k!='capabilities'},{k:v for k,v in values['zig'].items() if k!='capabilities'})
  pages=values['rust']['page_results']
  results.append({'id':q['id'],'equal':not delta,'ok':{n:v.get('ok') for n,v in values.items()},'response_keys':list(values['rust']),'phase_info':phase_info,'request_bytes':len(json.dumps(q)),'differences':delta,'page_observations':pages})
  requests.append(q)
  (scratch/'partial-results.json').write_text(json.dumps(results,indent=2)+'\n')
  print(q['id'], 'PASS' if not delta else 'FAIL',json.dumps(delta[:2]),flush=True)
finally:
 for peer in peers.values():peer.close()
with gzip.open(scratch/'requests.json.gz','wt') as f:json.dump(requests,f,separators=(',',':'))
binaries={name:hashlib.sha256((root/exe).read_bytes()).hexdigest() for name,exe in [('rust','target/release/examples/parity'),('zig','zig-out/bin/vt-oracle')]}
(scratch/'results.json').write_text(json.dumps({'binaries':binaries,'cases':results,'matched':sum(x['equal'] for x in results),'total':len(results),'comparison':'complete parsed JSON except static capabilities array; capabilities have identical members in different advertised order, retained below and in raw replies. No page/state/accounting field removed or normalized','capabilities':capabilities,'timings':'not measured'},indent=2)+'\n')
