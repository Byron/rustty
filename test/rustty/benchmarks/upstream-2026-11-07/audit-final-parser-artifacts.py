"""Read recorded replies only. Never launch an oracle or alter a fixture/classifier."""
import collections,copy,hashlib,json,pathlib
S=pathlib.Path('/private/tmp/rustty-reintegrate.leadij')
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def diff(a,b,p='$'):
 if type(a)!=type(b):return [p+':type']
 if isinstance(a,dict):
  if a.keys()!=b.keys():return [p+':keys']
  return sum((diff(a[k],b[k],p+'.'+k) for k in a),[])
 if isinstance(a,list):
  if len(a)!=len(b):return [p+':length']
  return sum((diff(x,y,p+f'[{i}]') for i,(x,y) in enumerate(zip(a,b))),[])
 return [] if a==b else [p]
rows=[];incomplete=[];other_categories=[];complete_directories=0
all_directories=sorted(p for p in (S/'final-parity/groups-2/failures').iterdir() if p.is_dir())
for d in all_directories:
 problems=[];parsed={};files={}
 for name in ['request.json','zig.json','rust.json','difference.txt']:
  f=d/name
  if not f.is_file():problems.append(name+': missing');continue
  files[name]={'sha256':sha(f),'bytes':f.stat().st_size}
  if not f.stat().st_size:problems.append(name+': empty');continue
  try:
   parsed[name]=json.loads(f.read_text()) if name.endswith('.json') else f.read_text()
  except (ValueError,UnicodeError) as error:problems.append(name+': '+str(error))
 if problems:
  incomplete.append({'artifact':str(d),'id':parsed.get('request.json',{}).get('id'),'problems':problems,'retained_files':files,'counted_as_complete':False});continue
 complete_directories+=1
 if not parsed['difference.txt'].startswith('response.parser.events'):
  other_categories.append({'artifact':str(d),'id':parsed['request.json']['id'],'difference':parsed['difference.txt']});continue
 z=parsed['zig.json'];r=parsed['rust.json'];q=parsed['request.json']
 original_z=copy.deepcopy(z);ze=z['parser']['events'];re=r['parser']['events'];i=j=0;skipped=[]
 while i<len(ze):
  if j<len(re) and ze[i]==re[j]:i+=1;j+=1;continue
  if (j<len(re) and ze[i]['kind'] in ['osc','osc_overflow'] and i+1<len(ze)
      and ze[i+1]==re[j] and re[j]['kind']=='execute' and re[j]['byte'] in [24,26]):
   skipped.append({'native_index':i,'cancel_byte':re[j]['byte'],'event':ze[i]});i+=1;continue
  break
 aligned=i==len(ze) and j==len(re)
 if aligned:
  indices={v['native_index'] for v in skipped};z['parser']['events']=[v for k,v in enumerate(ze) if k not in indices]
  for state in z['parser']['states']:state['events']-=sum(k<state['events'] for k in indices)
 z.pop('capabilities',None);r.pop('capabilities',None)
 remainder=diff(z,r)
 rows.append({'artifact':str(d),'id':q['id'],'hashes':{f:sha(d/f) for f in ['request.json','zig.json','rust.json','difference.txt']},'aligned':aligned,'native_events':len(ze),'rust_events':len(re),'skipped':skipped,'remaining_differences':remainder})
summary={'analysis':'artifact-only; align equal events first, permit only unmatched native OSC directly before matching CAN/SUB; adjust native state event offsets only; exclude root capabilities as Rust runner does; no comparator relaxation','case_count':len(rows),'all_fields_explained':all(not x['remaining_differences'] for x in rows),'extra_events':sum(len(x['skipped']) for x in rows),'cancel_bytes':dict(collections.Counter(y['cancel_byte'] for x in rows for y in x['skipped'])),'unexplained':[{'id':x['id'],'remaining':x['remaining_differences']} for x in rows if x['remaining_differences']],'cases':rows}
import datetime
summary.update(json.loads((S/'final-parser-source-attribution.json').read_text()))
summary['run_status']={'kind':'permanently_incomplete_initial_run','reason':'Root stopped group2 after confirming stale canceled-OSC observer; coordinator session10124 exited130 and waited for its process group. A new run follows the observer repair.','coordinator_exit_status':130,'partial_run_evidence_only':True,'full_archive_completion_claimed':False}
summary['artifact_integrity']={'saved_directories':len(all_directories),'complete_directories':complete_directories,'incomplete_directories':incomplete,'other_complete_categories':other_categories,'all_saved_directories_accounted_for':len(rows)+len(incomplete)+len(other_categories)==len(all_directories)}
summary['as_of_utc']=datetime.datetime.now(datetime.timezone.utc).isoformat()
summary['broad_shard_summary_at_audit']=json.loads((S/'final-parity/groups-2/summary.json').read_text())
summary['artifact_script_sha256']=sha(pathlib.Path(__file__))
(S/'final-parser-artifact-attribution.json').write_text(json.dumps(summary,indent=2)+'\n')
compact={k:v for k,v in summary.items() if k!='cases'}
compact['cases']=[{**{k:v for k,v in x.items() if k!='skipped'},
  'skipped_native_indices':[y['native_index'] for y in x['skipped']],
  'cancel_bytes':dict(collections.Counter(y['cancel_byte'] for y in x['skipped'])),
  'skipped_events_sha256':hashlib.sha256(json.dumps(x['skipped'],sort_keys=True,separators=(',',':')).encode()).hexdigest()} for x in rows]
(S/'final-parser-failure-map.json').write_text(json.dumps(compact,indent=2)+'\n')
print(json.dumps({k:v for k,v in compact.items() if k not in ['cases','source_proof']},indent=2))
