#!/usr/bin/env python3
"""Complete only the interrupted rest job with the reviewed buffered runner."""
import argparse,hashlib,importlib.util,json,shutil,subprocess,time,zlib
from pathlib import Path
P=Path('/private/tmp/rustty-reintegrate.leadij')
spec=importlib.util.spec_from_file_location('broad',P/'run-final-parity.py');broad=importlib.util.module_from_spec(spec);spec.loader.exec_module(broad)
ROOT=broad.ROOT
GROUPS=['input','page-layout','pages','snapshots','snapshot-wire']

def main():
    ap=argparse.ArgumentParser(description=__doc__);ap.add_argument('output',type=Path);args=ap.parse_args()
    out=args.output.resolve();out.mkdir(exist_ok=False);(out/'binaries').mkdir()
    first=json.loads((P/'final-parity/run.json').read_text());middle=json.loads((P/'final-parity-corrected-group2/run.json').read_text())
    guards_path=P/'final-buffered-rest-reuse-guards.json';guards=json.loads(guards_path.read_text());guard_sha=broad.digest(guards_path)
    assert not first['complete'] and not middle['complete']
    parser=[r for r in middle['runs'] if r['name'].startswith('parser-')]
    assert len(parser)==6 and sum(r['summary']['checked'] for r in parser)==2028
    assert all(r['complete'] and r['summary']['failures']==0 and not r['summary']['aborted'] and r['summary']['error'] is None and r['summary']['coverage_gaps']==[] for r in parser)
    paths={'runner':ROOT/'target/release/examples/parity-runner','rust':ROOT/'target/release/examples/parity','zig':ROOT/'zig-out/bin/vt-oracle'}
    frozen={}
    for key,path in paths.items():
        target=out/'binaries'/key;shutil.copy2(path,target);frozen[key]={'path':str(target),'source_path':str(path),'sha256':broad.digest(target)}
    assert frozen['runner']['sha256']==guards['expected_runner_sha256']
    assert frozen['runner']['sha256']!=middle['binaries']['runner']['sha256']
    for key in ['rust','zig']:assert frozen[key]['sha256']==middle['binaries'][key]['sha256']
    witness_path=P/guards['artifact_io_witness']['path'];assert broad.digest(witness_path)==guards['artifact_io_witness']['sha256'];witness=json.loads(witness_path.read_text())
    assert witness['completed'] and witness['all192_saved_files_exact']
    assert witness['identities']['before']['sha256']==middle['binaries']['runner']['sha256']
    assert witness['identities']['after']['sha256']==frozen['runner']['sha256']
    for key in ['rust','zig']:assert witness['identities'][key]['sha256']==frozen[key]['sha256']
    head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip();assert head==guards['expected_start_head']
    assert subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=ROOT,text=True)==''
    changed=subprocess.check_output(['git','diff','--name-only',middle['source_commit'],'HEAD'],cwd=ROOT,text=True).splitlines()
    assert changed==['test/rustty/parity-runner.rs'],changed
    source=guards['runner_source'];path=source['path'];before=subprocess.check_output(['git','show',middle['source_commit']+':'+path],cwd=ROOT)
    diff=subprocess.check_output(['git','diff','--no-color','--no-ext-diff','--full-index',middle['source_commit'],'HEAD','--',path],cwd=ROOT)
    assert hashlib.sha256(before).hexdigest()==source['before_sha256']
    assert broad.digest(ROOT/path)==source['after_sha256']
    assert hashlib.sha256(diff).hexdigest()==source['diff_sha256']
    manifest_path=ROOT/'test/rustty/fixtures/manifest.json';manifest=json.loads(manifest_path.read_text())
    assert broad.digest(manifest_path)==middle['fixture_manifest_sha256']==guards['expected_fixture_manifest_sha256']
    meta={'started':time.time(),'complete':False,'source_commit':head,'binaries':frozen,'runs':[],
          'source_guard_sha256':guard_sha,'runner_source_diff_verified':True,'fixture_manifest_sha256':broad.digest(manifest_path),
          'first_run_sha256':broad.digest(P/'final-parity/run.json'),'parser_run_sha256':broad.digest(P/'final-parity-corrected-group2/run.json'),
          'script_sha256':broad.digest(Path(__file__)),'shared_helper_sha256':broad.digest(P/'run-final-parity.py'),
          'artifact_io_witness':guards['artifact_io_witness'],'archive_provenance':{},
          'scope':'Complete replacement of the entire interrupted rest job; retained parser coverage is the six complete zero-failure corrected shards. Only runner artifact serialization buffering/flush changed; native/Rust oracles and all fixture bytes are identical.'}
    def save():(out/'run.json').write_text(json.dumps(meta,indent=2)+'\n')
    save();checksums={}
    for group in broad.GROUPS+['thorough']:
        p,header,items=broad.archive(group);entry=manifest[group]
        assert broad.digest(p)==middle['archive_provenance'][group]['sha256']==entry['sha256']
        assert (p.stat().st_size,len(items),sum(map(broad.variants,items)))==(entry['bytes'],entry['cases'],entry['comparisons'])
        for key in ['rust','zig']:assert entry['oracles'][key]==frozen[key]['sha256']
        for rel,expected in header['inputs'].items():
            if rel not in checksums:checksums[rel]=zlib.crc32((ROOT/rel).read_bytes())
            assert checksums[rel]==expected,(group,rel)
        for rel,expected in header['directories'].items():assert sorted(x.name for x in (ROOT/rel).iterdir() if x.is_file())==expected
        meta['archive_provenance'][group]={'sha256':entry['sha256'],'cases':len(items),'comparisons':entry['comparisons'],'native_probes':len(header['probes'])}
    smoke_path=ROOT/'test/rustty/smoke.json';assert broad.digest(smoke_path)==first['smoke']['sha256'];smoke=json.loads(smoke_path.read_text())
    count=sum(meta['archive_provenance'][g]['comparisons'] for g in GROUPS)+sum(map(broad.variants,smoke))+300;assert count==17170
    argv=[frozen['runner']['path'],'--no-build','--rust-bin',frozen['rust']['path'],'--zig-bin',frozen['zig']['path'],
          '--artifacts',str(out/'rest'),'--max-failures','100000','--timeout','60','--generated','100','--seed','0']+['--'+g for g in GROUPS]
    row={'name':'rest','argv':argv,'expected_comparisons':count,'groups':GROUPS,'generated':100,'seed':0,'smoke_comparisons':sum(map(broad.variants,smoke))};meta['runs']=[row];save()
    log=(out/'rest.log').open('w');row['started']=time.time();process=subprocess.Popen(argv,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,start_new_session=True);row['pid']=process.pid;save()
    try:
        while process.poll() is None:time.sleep(.2)
        row['exit_status']=process.returncode
    finally:
        if process.poll() is None:row['cancelled']=True;broad.stop(process)
        log.close();row['exit_status']=process.returncode;row['finished']=time.time();save()
    summary=json.loads((out/'rest/summary.json').read_text());row['summary']=summary
    row['complete']=process.returncode in (0,1) and not summary['aborted'] and summary['error'] is None and summary['coverage_gaps']==[] and summary['checked']==count and summary['failures']<100000
    meta.update(complete=row['complete'],finished=time.time(),actual_physical_comparisons=summary['checked'],raw_failures=summary['failures'],raw_full_parity=summary['full_parity']);save()
    assert row['complete']
    assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()==head
    assert subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=ROOT,text=True)==''
    assert broad.digest(manifest_path)==meta['fixture_manifest_sha256']
    for group,record in meta['archive_provenance'].items():assert broad.digest(ROOT/'test/rustty/fixtures'/f'{group}.jsonl.gz')==record['sha256']
    for binary in frozen.values():assert broad.digest(Path(binary['path']))==binary['sha256']
    assert broad.digest(ROOT/path)==source['after_sha256']
    assert broad.digest(guards_path)==guard_sha
    assert broad.digest(Path(__file__))==meta['script_sha256'] and broad.digest(P/'run-final-parity.py')==meta['shared_helper_sha256']
    meta['head_sources_fixtures_and_scripts_unchanged_at_completion']=True;save()
    argv=['python3',str(P/'classify-final-parity.py'),str(out/'rest')];result=subprocess.run(argv,capture_output=True,text=True)
    row['classifier']={'argv':argv,'status':result.returncode,'stdout':result.stdout,'stderr':result.stderr};meta['classifier_execution_complete']=result.returncode==0;save()
    assert meta['classifier_execution_complete'];print(json.dumps({'complete':True,'checked':count,'raw_failures':summary['failures']}))

if __name__=='__main__':main()
