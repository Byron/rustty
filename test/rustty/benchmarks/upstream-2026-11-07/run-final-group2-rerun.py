#!/usr/bin/env python3
"""Full replacement for interrupted group2; six parser shards plus its other groups, only Rust compares."""
import argparse,gzip,hashlib,importlib.util,json,shutil,subprocess,time,zlib
from pathlib import Path

P=Path('/private/tmp/rustty-reintegrate.leadij')
spec=importlib.util.spec_from_file_location('broad',P/'run-final-parity.py')
broad=importlib.util.module_from_spec(spec);spec.loader.exec_module(broad)
ROOT=broad.ROOT
WORKERS=6

def partition(items):
    bins=[[] for _ in range(WORKERS)];weights=[0]*WORKERS
    ranked=sorted(enumerate(items),key=lambda x:(-len(json.dumps(x[1],separators=(',',':')).encode()),x[0]))
    for index,item in ranked:
        target=min(range(WORKERS),key=lambda i:(weights[i],i))
        bins[target].append((index,item));weights[target]+=len(json.dumps(item,separators=(',',':')).encode())
    return [[item for _,item in sorted(bucket)] for bucket in bins],weights

def selfcheck():
    items=[{'request':{'id':str(i),'kind':'parser','data':'00'*(1+i%17)},'covers':[]} for i in range(676)]
    bins,weights=partition(items)
    ids=[item['request']['id'] for bucket in bins for item in bucket]
    assert len(bins)==6 and all(bins)
    assert len(ids)==len(set(ids))==676
    assert set(ids)=={str(i) for i in range(676)}
    assert sum(broad.variants(item) for bucket in bins for item in bucket)==2028
    assert partition(items)==(bins,weights)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output',type=Path,nargs='?');parser.add_argument('--selfcheck',action='store_true')
    args=parser.parse_args();selfcheck()
    if args.selfcheck:
        assert args.output is None
        print('Six disjoint deterministic shard selfchecks passed; no binaries invoked')
        return
    if args.output is None:parser.error('output directory required')
    out=args.output.resolve();out.mkdir(exist_ok=False);(out/'binaries').mkdir()
    first=json.loads((P/'final-parity/run.json').read_text())
    guard_path=P/'final-group2-reuse-guards.json';guards=json.loads(guard_path.read_text());guard_sha=broad.digest(guard_path)
    assert not first['complete'] and not first['coverage_complete']
    assert first['commit']=='03be726e243bdc235bd641420f511fc2b7792f8e'
    assert {r['name'] for r in first['runs'] if r.get('complete')}=={'groups-1','groups-3','thorough-cases'}
    assert json.loads((P/'final-parity-stopped.json').read_text())['coordinator_exit_status']==130
    paths={'runner':ROOT/'target/release/examples/parity-runner','rust':ROOT/'target/release/examples/parity','zig':ROOT/'zig-out/bin/vt-oracle'}
    frozen={}
    for name,path in paths.items():
        target=out/'binaries'/name;shutil.copy2(path,target)
        frozen[name]={'path':str(target),'source_path':str(path),'sha256':broad.digest(target)}
    assert frozen['zig']['sha256']==guards['expected_native_sha256']
    assert frozen['zig']['sha256']!=first['binaries']['zig']['sha256'],'observer rebuild required'
    for name in ['runner','rust']:
        assert frozen[name]['sha256']==first['binaries'][name]['sha256'],(name,'unexpected change beyond native observer; inspect before reusing unaffected coverage')
    meta={'started':time.time(),'complete':False,'all_parser_comparisons_passed':False,
          'source_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
          'script_sha256':broad.digest(Path(__file__)),'shared_helper_sha256':broad.digest(P/'run-final-parity.py'),
          'reuse_guard_sha256':guard_sha,'first_broad_run_sha256':broad.digest(P/'final-parity/run.json'),'binaries':frozen,'parser_workers':WORKERS,'max_total_workers':WORKERS+1,
          'archive_provenance':{},'runs':[],
          'scope':'Replaces the whole interrupted group2: six disjoint parser fixtures (all3 delivery modes) plus one rest job with input/page-layout/pages/snapshots/snapshot-wire, default smoke and seed0 generated100. Keeps initial groups1/3/thorough only under exact payload/source identity proof. No comparison in Python.'}
    def save():(out/'run.json').write_text(json.dumps(meta,indent=2)+'\n')
    assert meta['source_commit']==guards['expected_start_head']
    assert subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=ROOT,text=True)==''
    save();manifest_path=ROOT/'test/rustty/fixtures/manifest.json';manifest=json.loads(manifest_path.read_text())
    meta['fixture_manifest_sha256']=broad.digest(manifest_path);crcs={};parser_items=None
    assert meta['fixture_manifest_sha256']==guards['expected_fixture_manifest_sha256']
    for group in broad.GROUPS+['thorough']:
        path,header,items=broad.archive(group);entry=manifest[group]
        assert (broad.digest(path),path.stat().st_size,len(items),sum(map(broad.variants,items)))==(entry['sha256'],entry['bytes'],entry['cases'],entry['comparisons']),group
        for engine in ['rust','zig']:assert entry['oracles'][engine]==frozen[engine]['sha256'],(group,engine)
        for rel,expected in header['inputs'].items():
            if rel not in crcs:crcs[rel]=zlib.crc32((ROOT/rel).read_bytes())
            assert crcs[rel]==expected,(group,rel)
        for rel,expected in header['directories'].items():
            assert sorted(p.name for p in (ROOT/rel).iterdir() if p.is_file())==expected,(group,rel)
        old_bytes=subprocess.check_output(['git','show',first['commit']+':test/rustty/fixtures/'+group+'.jsonl.gz'],cwd=ROOT)
        old_rows=[json.loads(line) for line in gzip.decompress(old_bytes).splitlines()]
        payload_equal=old_rows[1:]==items
        if group!='parser':
            assert payload_equal,(group,'request/coverage payload changed; unchanged-shard reuse needs review')
        else:
            assert [x['request']['id'] for x in old_rows[1:]]==[x['request']['id'] for x in items]
            meta['parser_changed_requests']=[{'id':a['request']['id'],'before':a,'after':b} for a,b in zip(old_rows[1:],items) if a!=b]
            assert meta['parser_changed_requests']==[guards['expected_parser_change']],'Only the exact reviewed canceled-OSC seed may differ'
        assert old_rows[0]['probes']==header['probes'],(group,'native probe payload changed')
        assert old_rows[0]['directories']==header['directories'],(group,'corpus membership changed')
        meta['archive_provenance'][group]={'sha256':entry['sha256'],'cases':len(items),'comparisons':entry['comparisons'],'native_probes':len(header['probes']),
                                         'requests_and_covers_exact_equal_to_initial':payload_equal,'native_probe_payloads_exact_equal_to_initial':True,'corpus_membership_exact_equal_to_initial':True}
        del old_rows,old_bytes
        if group=='parser':
            assert not header['probes'],'External parser extraction would otherwise skip native probe validation'
            parser_items=items
    # The native observer is the sole non-test implementation change allowed
    # when reusing the completed non-parser runs. Root also reviews its diff.
    changed=subprocess.check_output(['git','diff','--name-only',first['commit'],'HEAD'],cwd=ROOT,text=True).splitlines()
    meta['changed_source_paths']=changed
    expected_files=guards['source_files']
    assert [x for x in changed if not x.startswith('test/rustty/fixtures/')]==sorted(expected_files),changed
    for path,expected in expected_files.items():
        before=subprocess.check_output(['git','show',first['commit']+':'+path],cwd=ROOT)
        diff=subprocess.check_output(['git','diff','--no-color','--no-ext-diff','--full-index',first['commit'],'HEAD','--',path],cwd=ROOT)
        assert hashlib.sha256(before).hexdigest()==expected['before_sha256'],path
        assert broad.digest(ROOT/path)==expected['after_sha256'],path
        assert hashlib.sha256(diff).hexdigest()==expected['diff_sha256'],path
    subprocess.run(['git','diff','--quiet',first['commit'],'HEAD','--','crates/rustty-vt/src','src'],cwd=ROOT,check=True)
    meta['exact_reviewed_source_diff_verified']=True
    assert parser_items is not None and len(parser_items)==676
    assert sum(map(broad.variants,parser_items))==2028
    ids=[f['request']['id'] for f in parser_items];assert len(set(ids))==676
    bins,weights=partition(parser_items)
    assert sorted(json.dumps(f,sort_keys=True) for b in bins for f in b)==sorted(json.dumps(f,sort_keys=True) for f in parser_items)
    for i,bucket in enumerate(bins):
        name=f'parser-{i+1}';fixture=out/f'{name}.json';fixture.write_text(json.dumps(bucket,separators=(',',':'))+'\n')
        argv=[frozen['runner']['path'],'--no-build','--rust-bin',frozen['rust']['path'],'--zig-bin',frozen['zig']['path'],
              '--fixtures',str(fixture),'--artifacts',str(out/name),'--max-failures','100000','--timeout','60']
        meta['runs'].append({'name':name,'argv':argv,'fixture_sha256':broad.digest(fixture),'ids':[f['request']['id'] for f in bucket],
                             'fixture_weight_bytes':weights[i],'expected_comparisons':sum(map(broad.variants,bucket))})
    parser_count=sum(row['expected_comparisons'] for row in meta['runs']);assert parser_count==2028
    rest_groups=['input','page-layout','pages','snapshots','snapshot-wire']
    smoke=json.loads((ROOT/'test/rustty/smoke.json').read_text())
    old_smoke=json.loads(subprocess.check_output(['git','show',first['commit']+':test/rustty/smoke.json'],cwd=ROOT))
    assert smoke==old_smoke
    rest_count=sum(meta['archive_provenance'][g]['comparisons'] for g in rest_groups)+sum(map(broad.variants,smoke))+300
    assert rest_count==17170
    argv=[frozen['runner']['path'],'--no-build','--rust-bin',frozen['rust']['path'],'--zig-bin',frozen['zig']['path'],
          '--artifacts',str(out/'rest'),'--max-failures','100000','--timeout','60','--generated','100','--seed','0']+['--'+g for g in rest_groups]
    meta['runs'].append({'name':'rest','argv':argv,'groups':rest_groups,'generated':100,'seed':0,'smoke_comparisons':sum(map(broad.variants,smoke)),'expected_comparisons':rest_count})
    meta['parser_unique_requests']=676;meta['parser_comparisons']=2028;meta['expected_physical_comparisons']=19198;meta['within_replacement_repeated_controls']=0;save()
    active=[]
    try:
        for row in meta['runs']:
            log=(out/(row['name']+'.log')).open('w');row['started']=time.time()
            process=subprocess.Popen(row['argv'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            row['pid']=process.pid
            active.append((process,log,row));print('START',row['name'],row['expected_comparisons'],flush=True)
        save()
        while active:
            for process,log,row in list(active):
                status=process.poll()
                if status is None:continue
                log.close();row.update(exit_status=status,finished=time.time());path=out/row['name']/'summary.json'
                row['summary']=json.loads(path.read_text()) if path.exists() else {'aborted':True}
                result=row['summary'];row['complete']=status in (0,1) and not result.get('aborted',True) and result.get('checked')==row['expected_comparisons'] and result.get('failures',100000)<100000
                active.remove((process,log,row));save();print('END',row['name'],result,flush=True)
            if active:time.sleep(.2)
    finally:
        for process,log,row in active:
            broad.stop(process);log.close();row.update(cancelled=True,exit_status=process.returncode,finished=time.time())
        save()
    meta['finished']=time.time();meta['complete']=all(row['complete'] for row in meta['runs'])
    meta['actual_physical_comparisons']=sum(row['summary'].get('checked',0) for row in meta['runs'])
    meta['raw_failures']=sum(row['summary'].get('failures',0) for row in meta['runs'])
    meta['all_replacement_comparisons_passed']=meta['complete'] and meta['raw_failures']==0
    meta['all_parser_comparisons_passed']=all(row['complete'] and row['summary']['failures']==0 for row in meta['runs'] if row['name'].startswith('parser-'))
    meta['raw_full_parity']=all(row['summary'].get('full_parity',False) for row in meta['runs'])
    for binary in frozen.values():assert broad.digest(Path(binary['path']))==binary['sha256']
    assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()==meta['source_commit']
    assert subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=ROOT,text=True)==''
    assert broad.digest(manifest_path)==meta['fixture_manifest_sha256']
    assert broad.digest(guard_path)==guard_sha
    assert broad.digest(Path(__file__))==meta['script_sha256']
    assert broad.digest(P/'run-final-parity.py')==meta['shared_helper_sha256']
    for group,archive in meta['archive_provenance'].items():
        assert broad.digest(ROOT/'test/rustty/fixtures'/f'{group}.jsonl.gz')==archive['sha256'],group
    for path,expected in expected_files.items():assert broad.digest(ROOT/path)==expected['after_sha256'],path
    meta['head_sources_fixtures_and_scripts_unchanged_at_completion']=True
    save();assert meta['complete'] and meta['actual_physical_comparisons']==19198
    for row in meta['runs']:
        argv=['python3',str(P/'classify-final-parity.py'),str(out/row['name'])]
        result=subprocess.run(argv,capture_output=True,text=True)
        row['classifier']={'argv':argv,'status':result.returncode,'stdout':result.stdout,'stderr':result.stderr}
        save()
    meta['classifier_execution_complete']=all(row['classifier']['status']==0 for row in meta['runs']);save()
    assert meta['classifier_execution_complete']
    print(json.dumps({k:meta[k] for k in ['complete','all_parser_comparisons_passed','all_replacement_comparisons_passed','actual_physical_comparisons','raw_failures','raw_full_parity']}))

if __name__=='__main__':main()
