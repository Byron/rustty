#!/usr/bin/env python3
"""Summarize saved Rust-runner failures and source ancestry, without rerunning comparisons."""
from pathlib import Path
import hashlib,json,subprocess
P=Path('/private/tmp/rustty-reintegrate.leadij'); OUT=P/'final-color-grid-review'
def git(*argv):return subprocess.check_output(['git',*argv])
def digest(data):return hashlib.sha256(data).hexdigest()
def diff(a,b,path='response'):
    if type(a)!=type(b):return [{'path':path,'native':a,'rust':b}]
    if isinstance(a,dict):
        rows=[]
        for k in sorted(a.keys()|b.keys()):
            if path=='response' and k in ('capabilities','encodings'):continue
            if k not in a or k not in b:rows.append({'path':path+'.'+k,'native':a.get(k),'rust':b.get(k)})
            else:rows.extend(diff(a[k],b[k],path+'.'+k))
        return rows
    if isinstance(a,list):
        rows=[]
        if len(a)!=len(b):rows.append({'path':path+'.length','native':len(a),'rust':len(b)})
        for i,(x,y) in enumerate(zip(a,b)):rows.extend(diff(x,y,f'{path}[{i}]'))
        return rows
    return [] if a==b else [{'path':path,'native':a,'rust':b}]
current={}
for shard in ['groups-1','groups-3']:
 for f in (P/'final-parity'/shard/'failures').glob('*/request.json'):
  r=json.loads(f.read_text())
  if r['id'].startswith(('protocol/colors/','grid/')):current[r['id']]=f.parent
rows=[]
for f in sorted((OUT/'baseline-replay/failures').glob('*/request.json')):
 r=json.loads(f.read_text());i=r['id'];cur=current[i];a=json.loads((cur/'zig.json').read_text());b=json.loads((cur/'rust.json').read_text());ba=json.loads((f.parent/'zig.json').read_text());bb=json.loads((f.parent/'rust.json').read_text())
 assert a==ba and b==bb, i
 group='color-numeric' if i.startswith('protocol/colors/') else 'semantic-selection-trimming' if i.startswith('grid/selectors/') else 'empty-vt-point-map'
 rows.append({'id':i,'category':group,'current_artifact':str(cur),'baseline_artifact':str(f.parent),'baseline_native_reply_exact_equal':True,'baseline_rust_reply_exact_equal':True,'request_sha256':digest((cur/'request.json').read_bytes()),'native_sha256':digest((cur/'zig.json').read_bytes()),'rust_sha256':digest((cur/'rust.json').read_bytes()),'differences':diff(a,b)})
assert len(rows)==49
sources=[]
for path in ['crates/rustty-vt/src/color.rs','crates/rustty-vt/src/selection.rs']:
 a=git('show','c06197283:'+path);b=Path(path).read_bytes();assert a==b
 sources.append({'path':path,'starting_commit':'c06197283','starting_and_current_blob':git('rev-parse','c06197283:'+path).decode().strip(),'sha256':digest(a),'whole_file_identical':True})
path='crates/rustty-vt/src/formatter.rs';start='        if options.emit == Format::Vt && !options.trim {';end='        Some(())';parts=[]
for rev in ['c06197283','HEAD']:
 text=git('show',rev+':'+path).decode();part=text[text.index(start):text.index(end,text.index(start))];parts.append(part)
assert parts[0]==parts[1]
sources.append({'path':path,'starting_commit':'c06197283','section':'Screen::format_into trailing untrimmed VT rows before Some(())','section_sha256':digest(parts[0].encode()),'section_identical':True,'section':parts[0],'other_current_changes':'Only screen_extra effective margin and its callers changed; this test has empty extra options and identical formatted output.'})
fixes=[]
for h in ['73768913b','13b5ab204','bd1c9cc8c']:
 subprocess.run(['git','merge-base','--is-ancestor',h,'6ba144158'],check=True)
 fixes.append({'commit':git('rev-parse',h).decode().strip(),'summary':git('show','-s','--format=%ad %s','--date=iso',h).decode().strip(),'ancestor_of_old_base':True})
result={'raw_failures':49,'categories':{'color-numeric':40,'semantic-selection-trimming':6,'empty-vt-point-map':3},'baseline_replay':json.loads((OUT/'run.json').read_text()),'all_current_and_baseline_native_replies_exact_equal':True,'all_current_and_baseline_rust_replies_exact_equal':True,'sources':sources,'older_native_commits':fixes,'dispositions':{
'color-numeric':'Pre-existing older gap: unsigned removes internal underscores; signed_index accepts + and -0. Native digits-only parsing rejects them. Invalid reset index consequently resets all instead of one palette slot; RGB width still counts underscore bytes in Rust. All40 failures are color parser/PTTY response/current palette consequences. Rust color.rs exactly matches starting source.',
'semantic-selection-trimming':'Pre-existing older gap13b5ab204: Rust limits whitespace scan by row only, allows crossing semantic end column into following output, returns start after end. Native end_pin.before(p) stops that scan and returns null. Both full source and current/baseline responses match.',
'empty-vt-point-map':'Pre-existing mapping-only difference: empty8-column VT export emits the same CRLF bytes; with no previous map point, native retains rightmost x=7 from br.up(count-i), Rust creates fallback PagePosition x=0. Subsequent newlines use x=0 in both. Untrimmed VT content formatter maps differ; ordinary state/bytes are equal. This trailing-row block existed before the incoming range and is unchanged fromc061.'},'failures':rows,'scope':'Manual baseline/source disposition only; raw runner summaries and strict pointer-record classifier are unchanged.'}
(OUT/'review.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({'failures':len(rows),'bytes':(OUT/'review.json').stat().st_size,'sha256':digest((OUT/'review.json').read_bytes())}))
