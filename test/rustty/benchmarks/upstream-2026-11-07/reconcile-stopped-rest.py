"""Read stopped-run artifacts; reuse already validated immutable records."""
import collections,datetime,hashlib,importlib.util,json,pathlib
S=pathlib.Path('/private/tmp/rustty-reintegrate.leadij');root=S/'final-parity-corrected-group2/rest';prior_path=S/'final-rest-independent-attribution.json';prior=json.loads(prior_path.read_text());known={x['artifact']:x for x in prior['cases']}
def sha(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024*1024):h.update(b)
 return h.hexdigest()
source=S/'classify-final-parity.py';spec=importlib.util.spec_from_file_location('strict_classifier',source);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
rows=[];incomplete=[];new=0
for p in sorted((root/'failures').iterdir()):
 files={n:p/n for n in ['request.json','zig.json','rust.json','difference.txt']}
 if str(p) in known and all(f.is_file() and f.stat().st_size for f in files.values()):
  rows.append({**known[str(p)],'validation':'reused previous complete per-ID artifact audit; immutable raw files retained'});continue
 issues=[];values={};manifest={}
 for name,f in files.items():
  if not f.is_file():issues.append(name+': missing');continue
  manifest[name]={'bytes':f.stat().st_size,'sha256':sha(f)}
  try:values[name]=json.loads(f.read_text()) if name.endswith('.json') else f.read_text()
  except (ValueError,UnicodeError) as e:issues.append(name+': '+str(e))
 if issues:
  incomplete.append({'artifact':str(p),'id':values.get('request.json',{}).get('id'),'problems':issues,'retained_files':manifest,'counted_as_complete':False});continue
 q=values['request.json'];z=values['zig.json'];r=values['rust.json'];c=m.classify(z,r)
 if q.get('kind')!='snapshot' and z.get('encodings')!=r.get('encodings'):
  c['classification']='needs-review';c['remaining_paths'].append('response.encodings');c['remaining_count']+=1
 rows.append({'id':q['id'],'artifact':str(p),'request_kind':q.get('kind','input'),'first_runner_path':values['difference.txt'].split(':',1)[0],**c,'hashes':{n:x['sha256'] for n,x in manifest.items()},'validation':'newly read and classified after process termination'});new+=1
out={'as_of_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'status':'permanently incomplete second rest attempt; coordinator exited130; rest PID16572 terminated with-15 and waited','partial_run_evidence_only':True,'full_archive_completion_claimed':False,'highest_complete_artifact_directory_index':max(int(pathlib.Path(x['artifact']).name) for x in rows),'completed_comparison_lower_bound_definition':'Highest completed failure artifact index proves at least that many comparisons reached artifact completion; not an exact all-attempt execution count.','runner_summary':json.loads((root/'summary.json').read_text()),'reused_previous_complete_count':len(rows)-new,'newly_validated_complete_count':new,'complete_failure_count':len(rows),'counts':dict(collections.Counter(x['classification'] for x in rows)),'incomplete_directories':incomplete,'all_saved_directories_accounted_for':len(rows)+len(incomplete)==len(list((root/'failures').iterdir())),'prior_audit':{'path':str(prior_path),'sha256':sha(prior_path)},'classification_source':{'path':str(source),'sha256':sha(source)},'source_identity':'final-rest-pointer-source-identity.json','cases':rows}
(S/'final-rest-stopped-attribution.json').write_text(json.dumps(out,indent=2)+'\n')
summary={k:v for k,v in out.items() if k!='cases'};summary['attribution_sha256']=sha(S/'final-rest-stopped-attribution.json');(S/'final-rest-stopped-summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
