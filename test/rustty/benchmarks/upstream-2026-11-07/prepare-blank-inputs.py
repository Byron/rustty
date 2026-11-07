from pathlib import Path
import json,hashlib
s=Path('/private/tmp/rustty-reintegrate.leadij'); configs=s/'stream-configs'; dest=s/'blank-inputs';dest.mkdir(exist_ok=True); manifest=[]
def config(name,path,cols=120,rows=80):
 c={'name':name,'data':str(path),'cols':cols,'rows':rows,'scrollback_bytes':10000,'grapheme_width':'legacy'}
 (configs/(name+'.json')).write_text(json.dumps(c,indent=2)+'\n')
def save(name,data,parent,cols=120,rows=80,transform='none'):
 p=dest/(name+'.vt');p.write_bytes(data);config(name,p,cols,rows)
 manifest.append({'name':name,'parent':str(parent),'transform':transform,'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'ED2':data.count(b'\x1b[2J'),'ED22':data.count(b'\x1b[22J'),'EL2':data.count(b'\x1b[2K'),'EL0':data.count(b'\x1b[K'),'CRLF':data.count(b'\r\n')})
for kind in ['2','22','rate','lines']:
 for cols,rows in [(120,80),(120,12),(300,100)]:
  name=f'erase-{kind}-{cols}x{rows}'; parent=s/'upstream-corpora'/(name+'.vt');config(name,parent,cols,rows)
  data=parent.read_bytes()[:1024*1024];data=data[:data.rfind(b'\r\n')+2]
  save('blank-prefix-'+name,data,parent,cols,rows,'first <=1MiB ending at complete CRLF redraw')
for name in ['wide','wide-spaces','mixed-wide']:config(name,s/'upstream-corpora'/(name+'.vt'))
parent=s/'upstream-corpora/plain.vt';save('plain-styled-protected',b'\x1b[1;31;44m\x1b[1"q'+parent.read_bytes(),parent,transform='prepend bold red/blue SGR and protected-character mode')
parent=dest/'blank-prefix-erase-2-120x80.vt'; save('erase-background',parent.read_bytes().replace(b'\x1b[2J',b'\x1b[44m\x1b[2J'),parent,transform='set indexed-blue background before each ED2')
(dest/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
# Existing ED prefixes remain exactly identical for direct port comparisons.
for c in (configs).glob('ed-prefix-*.json'):
 name=c.name.removeprefix('ed-prefix-').removesuffix('.json');old=Path(json.loads(c.read_text())['data']).read_bytes();assert old==(dest/('blank-prefix-'+name+'.vt')).read_bytes()
