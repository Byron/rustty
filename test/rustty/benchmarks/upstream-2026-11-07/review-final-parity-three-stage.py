#!/usr/bin/env python3
"""Join complete retained shards; preserve and exclude the interrupted attempt."""
from collections import Counter
import gzip,hashlib,importlib.util,json,subprocess
from pathlib import Path
P=Path('/private/tmp/rustty-reintegrate.leadij');OLD=P/'final-parity';NEW=P/'final-parity-corrected-group2';LAST=P/'final-parity-buffered-rest'
spec=importlib.util.spec_from_file_location('broad',P/'run-final-parity.py');broad=importlib.util.module_from_spec(spec);spec.loader.exec_module(broad)
def read(path):return json.loads(path.read_text())
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    classifier_sha=sha(P/'classify-final-parity.py')
    assert classifier_sha=='e94ff33449f805f3d672b4d1c169c4bdf6746a160a145689f91ff77ab12edee4'
    first=read(OLD/'run.json');new=read(NEW/'run.json');last=read(LAST/'run.json');stopped=read(P/'final-parser-stopped-summary.json');second_stop=read(P/'final-rest-stopped-summary.json')
    assert not first['complete'] and first['commit']=='03be726e243bdc235bd641420f511fc2b7792f8e'
    assert not new['complete']
    parsers=[r for r in new['runs'] if r['name'].startswith('parser-')]
    assert len(parsers)==6 and sum(r['summary']['checked'] for r in parsers)==2028
    assert all(r['complete'] and r['summary']['failures']==0 for r in parsers)
    assert last['complete'] and last['actual_physical_comparisons']==17170
    assert last['head_sources_fixtures_and_scripts_unchanged_at_completion']
    assert last['runner_source_diff_verified'] and last['classifier_execution_complete']
    assert new['exact_reviewed_source_diff_verified']
    assert second_stop['partial_run_evidence_only'] and not second_stop['full_archive_completion_claimed']
    assert second_stop['complete_failure_count']==604 and len(second_stop['incomplete_directories'])==1
    assert second_stop['highest_complete_artifact_directory_index']==11683
    assert stopped['run_status']['partial_run_evidence_only']
    assert stopped['case_count']==1152 and stopped['artifact_integrity']['saved_directories']==1153
    assert stopped['all_fields_explained'] and not stopped['unexplained']
    assert len(stopped['artifact_integrity']['incomplete_directories'])==1
    for group,meta in new['archive_provenance'].items():
        assert meta['native_probe_payloads_exact_equal_to_initial'] and meta['corpus_membership_exact_equal_to_initial']
        if group!='parser':assert meta['requests_and_covers_exact_equal_to_initial']
    assert new['parser_changed_requests']==[read(P/'final-group2-reuse-guards.json')['expected_parser_change']]
    assert sha(P/'final-group2-reuse-guards.json')==new['reuse_guard_sha256']
    assert sha(P/'run-final-group2-rerun.py')==new['script_sha256']
    assert sha(P/'run-final-parity.py')==new['shared_helper_sha256']
    assert sha(OLD/'run.json')==new['first_broad_run_sha256']
    for label,run in [('initial',first),('corrected-parser',new),('buffered-rest',last)]:
        for binary in run['binaries'].values():assert sha(Path(binary['path']))==binary['sha256'],(label,binary)
    for key in ['rust','runner']:assert first['binaries'][key]['sha256']==new['binaries'][key]['sha256']
    for key in ['rust','zig']:assert new['binaries'][key]['sha256']==last['binaries'][key]['sha256']
    assert last['binaries']['runner']['sha256']!=new['binaries']['runner']['sha256']
    assert sha(P/'final-buffered-rest-reuse-guards.json')==last['source_guard_sha256']
    assert sha(P/'run-final-buffered-rest.py')==last['script_sha256']
    assert sha(P/'run-final-parity.py')==last['shared_helper_sha256']
    assert sha(OLD/'run.json')==last['first_run_sha256'] and sha(NEW/'run.json')==last['parser_run_sha256']
    witness_path=P/last['artifact_io_witness']['path'];assert sha(witness_path)==last['artifact_io_witness']['sha256']
    witness=read(witness_path);assert witness['completed'] and witness['all192_saved_files_exact']
    assert witness['identities']['after']['sha256']==last['binaries']['runner']['sha256']
    assert witness['identities']['before']['sha256']==new['binaries']['runner']['sha256']
    assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=broad.ROOT,text=True).strip()==last['source_commit']
    assert subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=broad.ROOT,text=True)==''
    assert sha(broad.ROOT/'test/rustty/fixtures/manifest.json')==new['fixture_manifest_sha256']==last['fixture_manifest_sha256']
    # Recompute final configured/unique counts, including the strengthened seed.
    all_items={}
    for group in broad.GROUPS+['thorough']:
        path,header,items=broad.archive(group);assert sha(path)==new['archive_provenance'][group]['sha256']==last['archive_provenance'][group]['sha256'];all_items[group]=items
    smoke=read(broad.ROOT/'test/rustty/smoke.json')
    configured_items=smoke+[x for group in broad.GROUPS for x in all_items[group]]
    configured=broad.workload_counts(configured_items,100)
    unique=broad.workload_counts(configured_items+all_items['thorough'],100)
    assert configured=={'workloads':22027,'comparisons':61825,'unique_workloads':22027,'unique_comparison_variants':61825}
    assert unique['unique_workloads']==22106 and unique['unique_comparison_variants']==62062
    evidence={};evidence_files=[]
    for relative,key in [('final-protocol-residual-attribution.json','records'),('final-color-grid-review/review.json','failures'),('final-stream-gap-attribution.json','cases')]:
        path=P/relative;doc=read(path);evidence_files.append({'path':relative,'sha256':sha(path)})
        for r in doc[key]:
            if key=='records':
                assert r['current_rust_equals_baseline_raw_reply'] and r['current_native_equals_baseline_raw_reply']
                artifact=P/r['current_directory'];hashes={'request.json':r['request_sha256'],'zig.json':r['native_sha256'],'rust.json':r['current_rust_sha256']}
                category='older-'+r['family']
            elif key=='failures':
                assert r['baseline_native_reply_exact_equal'] and r['baseline_rust_reply_exact_equal']
                artifact=Path(r['current_artifact']);hashes={'request.json':r['request_sha256'],'zig.json':r['native_sha256'],'rust.json':r['rust_sha256']};category='older-'+r['category']
            else:
                assert doc['all_baseline_complete_replies_equal'];artifact=Path(r['artifact']);hashes=r['hashes'];category=r['category']
            assert r['id'] not in evidence
            evidence[r['id']]={'evidence':relative,'artifact':str(artifact),'hashes':hashes,'category':category}
    retained=[('initial',OLD,first,r) for r in first['runs'] if r['name'] in ['groups-1','groups-3','thorough-cases']]
    assert len(retained)==3
    retained += [('corrected-parser',NEW,new,r) for r in parsers]
    retained += [('buffered-rest',LAST,last,r) for r in last['runs']]
    assert len(retained)==10
    records=[];counts=Counter();strict=Counter();unknown=[];totals=0;used=set();runs=[]
    pointer_note=P/'optional-snapshot-record-note.md'
    for label,base,run,r in retained:
        s=r['summary'];assert r['complete'] and not s['aborted'] and s['error'] is None and s['coverage_gaps']==[]
        assert s['checked']==r['expected_comparisons']
        assert read(base/r['name']/'summary.json')==s
        classification_path=base/r['name']/'classification.json';classification_sha=sha(classification_path)
        c=read(classification_path);assert c['runner_summary']==s and len(c['failures'])==s['failures']
        totals+=s['checked'];runs.append({'origin':label,'name':r['name'],'source_commit':run.get('commit',run.get('source_commit')),'summary':s,'summary_sha256':sha(base/r['name']/'summary.json'),'classification_path':str(classification_path),'classification_sha256':classification_sha})
        for f in c['failures']:
            i=f['id'];artifact=Path(f['artifact']);hashes={n:sha(artifact/n) for n in ['request.json','zig.json','rust.json','difference.txt']};assert read(artifact/'request.json')['id']==i
            strict[f['classification']]+=1
            proof=evidence.get(i)
            if proof is not None:
                assert Path(proof['artifact'])==artifact,(i,'wrong run binding')
                assert all(hashes[k]==v for k,v in proof['hashes'].items()),i
                used.add(i)
            if f['classification']=='existing-pointer-shape-record-only':
                category='existing-pointer-shape-record-only'
                proof={'evidence':'optional-snapshot-record-note.md','source_evidence_sha256':sha(pointer_note),'classification_file':str(classification_path),'classification_sha256':classification_sha,'classifier_source_sha256':classifier_sha}
            elif proof is not None:category=proof['category']
            else:category='unexplained';unknown.append({'id':i,'artifact':str(artifact)})
            counts[category]+=1;records.append({'id':i,'origin':label,'shard':r['name'],'artifact':str(artifact),'hashes':hashes,'strict_classification':f['classification'],'category':category,'evidence':proof})
    assert set(evidence)==used,'Missing a known baseline case from retained completed shards'
    assert totals==62424
    raw_failures=sum(r['summary']['failures'] for r in runs);assert raw_failures==len(records)
    failure_ids={r['id'] for r in records};assert len(failure_ids)==raw_failures
    result={
      'coverage_complete':True,'full_cross_engine_parity':False,'initial_full_run_complete':False,
      'scope':'Composite complete configured coverage from initial groups1/3/thorough at03be, six corrected parser shards atca7e, and full buffered rest at96776. Both partial attempts are preserved and wholly excluded from retained coverage.',
      'initial_run_sha256':sha(OLD/'run.json'),'corrected_parser_run_sha256':sha(NEW/'run.json'),'buffered_rest_run_sha256':sha(LAST/'run.json'),'joining_script_sha256':sha(Path(__file__)),'classifier_source_sha256':classifier_sha,
      'configured_invocation_workloads':configured['workloads'],'configured_invocation_comparisons':configured['comparisons'],
      'configured_unique_workloads':configured['unique_workloads'],'configured_unique_comparison_variants':configured['unique_comparison_variants'],
      'all_suite_unique_workloads':unique['unique_workloads'],'all_suite_unique_comparison_variants':unique['unique_comparison_variants'],
      'retained_coverage_physical_comparisons':totals,'repeated_smoke_comparisons':totals-unique['unique_comparison_variants'],
      'retained_raw_passes':totals-raw_failures,'retained_raw_failures':raw_failures,'unique_raw_failure_ids':len(failure_ids),
      'strict_classifier_counts':dict(strict),'strict_classifier_all_explained':strict['needs-review']==0,
      'manual_attribution_complete':not unknown,'manual_attribution_counts':dict(counts),'unexplained':unknown,
      'corrected_parser':{'requests':676,'comparisons':2028,'failures':0,'seed_change':new['parser_changed_requests'][0]['id']},
      'initial_partial_attempt':{'completed_comparison_lower_bound':11572,'exact_checked_count':None,'complete_failure_records':1152,'interrupted_artifact_records':1,'source':'final-parser-stopped-summary.json','sha256':sha(P/'final-parser-stopped-summary.json'),'excluded_from_retained_coverage':True},
      'second_partial_attempt':{'completed_comparison_lower_bound':11683,'exact_checked_count':None,'complete_failure_records':604,'interrupted_artifact_records':1,'source':'final-rest-stopped-summary.json','sha256':sha(P/'final-rest-stopped-summary.json'),'excluded_from_retained_coverage':True},
      'all_attempts_scope':'Configured-suite attempts only; excludes focused regressions, baseline replays and artifact-I/O witness checks.',
      'all_attempts_completed_comparison_lower_bound':totals+11572+11683,'all_attempts_exact_comparison_total_known':False,
      'runs':runs,'binary_provenance':{'initial':first['binaries'],'corrected-parser':new['binaries'],'buffered-rest':last['binaries']},
      'reuse_proof':{'buffered_source_guard':'final-buffered-rest-reuse-guards.json','buffered_guard_sha256':last['source_guard_sha256'],'artifact_io_witness':last['artifact_io_witness'],'source_guard':'final-group2-reuse-guards.json','sha256':new['reuse_guard_sha256'],'all12_archive_payload_guards':new['archive_provenance'],'native_probes_regeneration':'final-corrected-fixture-validation.json','sha256_fixture_validation':sha(P/'final-corrected-fixture-validation.json')},
      'evidence':evidence_files,'failures':records,
      'interpretation':'Pointer-record classification and baseline attribution do not turn raw failures into passes. The global exhaustive/coverage gate was not requested or run. Neither stopped partial attempt has an exact final checked summary; neither is counted as complete.'}
    full_path=P/'final-parity-review.json'
    full_path.write_text(json.dumps(result,indent=2)+'\n')
    summary={k:v for k,v in result.items() if k!='failures'}
    summary['full_report']={'path':full_path.name,'bytes':full_path.stat().st_size,'sha256':sha(full_path)}
    if full_path.stat().st_size>2*1024*1024:
        compressed=P/'final-parity-review.json.gz';compressed.write_bytes(gzip.compress(full_path.read_bytes(),mtime=0))
        summary['full_report']['gzip']={'path':compressed.name,'bytes':compressed.stat().st_size,'sha256':sha(compressed)}
    (P/'final-parity-review-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k not in ['runs','binary_provenance','reuse_proof','evidence','failures']}))
    assert not unknown,'Unattributed raw failures require investigation'

if __name__=='__main__':main()
