#!/usr/bin/env python3
"""Run Rust comparisons after final fixture handoff; Python only orchestrates."""
from pathlib import Path
import argparse, gzip, hashlib, json, os, shutil, signal, subprocess, time, zlib

ROOT = Path('/Users/byron/dev/github.com/ghostty-org/ghostty')
SCRATCH = Path('/private/tmp/rustty-reintegrate.leadij')
GROUPS = ['input', 'parser', 'grid', 'page-layout', 'pages', 'unicode',
          'snapshots', 'snapshot-wire', 'protocols', 'corpus', 'osc']
SHARDS = [
    ['protocols'],
    ['input', 'snapshot-wire', 'pages', 'parser', 'page-layout', 'snapshots'],
    ['corpus', 'grid', 'osc', 'unicode'],
]
MAX_WORKERS = 3


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def variants(fixture):
    q = fixture['request']
    error = q.get('expected_error')
    terminal = q.get('kind', 'terminal') in ['terminal', 'input', 'parser', 'snapshot']
    return 1 if (isinstance(error, str) and error) or not terminal else 3


def unique_requests(fixtures):
    # Identity is exact named request JSON; covers are coverage metadata only.
    return {json.dumps(f['request'], sort_keys=True, separators=(',', ':')): f
            for f in fixtures}


def workload_counts(fixtures, generated):
    unique = unique_requests(fixtures)
    return {
        'workloads': len(fixtures) + generated,
        'comparisons': sum(map(variants, fixtures)) + generated * 3,
        'unique_workloads': len(unique) + generated,
        'unique_comparison_variants': sum(map(variants, unique.values())) + generated * 3,
    }


def selfcheck():
    flat = [group for shard in SHARDS for group in shard]
    assert len(SHARDS) == MAX_WORKERS == 3
    assert len(flat) == len(set(flat)) == len(GROUPS) and set(flat) == set(GROUPS)
    fixtures = [{'request': {'id': 'a'}, 'covers': []},
                {'request': {'id': 'b', 'kind': 'page_layout'}, 'covers': []},
                {'request': {'id': 'c', 'expected_error': 'bad'}, 'covers': []}]
    assert [variants(f) for f in fixtures] == [3, 1, 1]
    repeated = fixtures * 3
    assert sum(map(variants, repeated)) == 15
    assert len(unique_requests(repeated)) == 3
    assert sum(map(variants, unique_requests(repeated).values())) == 5
    # Configured invocation counts duplicates once per supplied fixture, while
    # unique counts deduplicate exact requests and exclude thorough.
    configured_fixtures = fixtures + fixtures[:1]
    configured = workload_counts(configured_fixtures, generated=2)
    assert configured == {'workloads': 6, 'comparisons': 14,
                          'unique_workloads': 5, 'unique_comparison_variants': 11}
    thorough = [fixtures[-1], {'request': {'id': 'd'}, 'covers': []}]
    all_suites = workload_counts(configured_fixtures + thorough, generated=2)
    assert all_suites == {'workloads': 8, 'comparisons': 18,
                         'unique_workloads': 6, 'unique_comparison_variants': 14}
    # Three group shards repeat smoke; thorough's explicit fixture replaces it.
    smoke_count = sum(map(variants, fixtures))
    physical = configured['comparisons'] + smoke_count * 2 + sum(map(variants, thorough))
    assert physical == 28
    assert physical - all_suites['unique_comparison_variants'] == 14


def archive(group):
    path = ROOT / 'test/rustty/fixtures' / (group + '.jsonl.gz')
    with gzip.open(path, 'rt') as f:
        header = json.loads(next(f))
        items = [json.loads(line) for line in f]
    assert header['format'] == 1 and len(items) == header['cases'], group
    return path, header, items


def stop(process):
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('output', type=Path, nargs='?')
    ap.add_argument('--selfcheck', action='store_true')
    args = ap.parse_args()
    selfcheck()
    if args.selfcheck:
        assert args.output is None
        print('Shard membership/count selfchecks passed; no binaries invoked')
        return
    if args.output is None:
        ap.error('output directory required')
    out = args.output.resolve()
    out.mkdir(exist_ok=False)
    sources = {'runner': ROOT / 'target/release/examples/parity-runner',
               'rust': ROOT / 'target/release/examples/parity',
               'zig': ROOT / 'zig-out/bin/vt-oracle'}
    (out / 'binaries').mkdir()
    binaries = {}
    for key, source in sources.items():
        binaries[key] = out / 'binaries' / key
        shutil.copy2(source, binaries[key])
    meta = {'started': time.time(), 'complete': False, 'coverage_complete': False,
            'full_parity': False, 'total_raw_failures': 0, 'max_workers': MAX_WORKERS,
            'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
            'binaries': {k: {'path': str(p), 'source_path': str(sources[k]), 'sha256': digest(p)}
                         for k, p in binaries.items()}, 'archives': {}, 'runs': []}
    def save():
        (out / 'run.json').write_text(json.dumps(meta, indent=2) + '\n')
    save()
    smoke_path = ROOT / 'test/rustty/smoke.json'
    smoke = json.loads(smoke_path.read_text())
    smoke_count = sum(map(variants, smoke))
    meta['smoke'] = {'sha256': digest(smoke_path), 'fixtures': len(smoke),
                     'comparisons_per_shard': smoke_count, 'group_shards': 3,
                     'extra_repeated_comparisons': smoke_count * 2}
    manifest_path = ROOT / 'test/rustty/fixtures/manifest.json'
    manifest = json.loads(manifest_path.read_text())
    meta['fixture_manifest_sha256'] = digest(manifest_path)
    checksums, all_items = {}, {}
    for group in GROUPS + ['thorough']:
        path, header, items = archive(group)
        all_items[group] = items
        count = sum(map(variants, items))
        source = manifest[group]
        for engine in ('rust', 'zig'):
            assert source['oracles'][engine] == meta['binaries'][engine]['sha256'], (
                group, engine, 'fixture manifest oracle differs from frozen current binary')
        assert (digest(path), path.stat().st_size, len(items), count) == (
            source['sha256'], source['bytes'], source['cases'], source['comparisons']), group
        for rel, expected in header['inputs'].items():
            if rel not in checksums:
                checksums[rel] = zlib.crc32((ROOT / rel).read_bytes())
            assert checksums[rel] == expected, (group, rel, 'stale fixture provenance')
        for rel, expected in header['directories'].items():
            assert sorted(p.name for p in (ROOT / rel).iterdir() if p.is_file()) == expected, (group, rel)
        meta['archives'][group] = {'sha256': digest(path), 'cases': len(items),
                                   'comparisons': count, 'probes': len(header['probes']),
                                   'oracles': source['oracles']}
        if group == 'thorough':
            assert not header['probes'], 'new thorough probes need native verification before extraction'
    thorough = out / 'thorough-fresh.json'
    thorough.write_text(json.dumps(all_items['thorough'], separators=(',', ':')) + '\n')
    meta['thorough_extracted_sha256'] = digest(thorough)
    configured_fixtures = smoke + [item for group in GROUPS for item in all_items[group]]
    configured = workload_counts(configured_fixtures, generated=100)
    meta['configured_invocation_workloads'] = configured['workloads']
    meta['configured_invocation_comparisons'] = configured['comparisons']
    meta['configured_unique_workloads'] = configured['unique_workloads']
    meta['configured_unique_comparison_variants'] = configured['unique_comparison_variants']
    meta['configured_definition'] = 'one smoke + all11 configured archive groups + generated100 seed0; excludes thorough'
    fixtures = configured_fixtures + all_items['thorough']
    all_suites = workload_counts(fixtures, generated=100)
    generated_ids = {f'generated/0/{i}' for i in range(100)}
    assert not generated_ids.intersection(f['request']['id'] for f in fixtures)
    meta['unique_workloads'] = all_suites['unique_workloads']
    meta['unique_comparison_variants'] = all_suites['unique_comparison_variants']
    meta['unique_definition'] = 'exact named request JSON, ignoring covers; generated seed0 adds100 distinct cases'
    plans = []
    for index, shard in enumerate(SHARDS):
        flags = ['--' + group for group in shard]
        generated = 100 if index == 1 else 0
        if generated:
            flags += ['--generated', '100', '--seed', '0']
        count = smoke_count + sum(meta['archives'][g]['comparisons'] for g in shard) + generated * 3
        plans.append({'name': f'groups-{index + 1}', 'groups': shard, 'generated': generated,
                      'smoke_controls': smoke_count, 'expected_comparisons': count, 'flags': flags})
    plans.append({'name': 'thorough-cases', 'groups': ['thorough'], 'generated': 0,
                  'smoke_controls': 0, 'expected_comparisons': meta['archives']['thorough']['comparisons'],
                  'flags': ['--fixtures', str(thorough)]})
    meta['expected_physical_comparisons'] = sum(p['expected_comparisons'] for p in plans)
    assert meta['configured_invocation_comparisons'] == (
        smoke_count + sum(meta['archives'][g]['comparisons'] for g in GROUPS) + 300)
    assert meta['expected_physical_comparisons'] == (
        meta['configured_invocation_comparisons'] + smoke_count * 2
        + meta['archives']['thorough']['comparisons'])
    meta['repeated_comparison_variants'] = meta['expected_physical_comparisons'] - meta['unique_comparison_variants']
    assert meta['repeated_comparison_variants'] >= smoke_count * 2
    save()
    pending, active = list(plans), []
    try:
        while pending or active:
            while pending and len(active) < MAX_WORKERS:
                row = pending.pop(0)
                flags = row.pop('flags')
                artifacts = out / row['name']
                row['argv'] = [str(binaries['runner']), '--no-build', '--rust-bin', str(binaries['rust']),
                               '--zig-bin', str(binaries['zig']), '--max-failures', '100000',
                               '--timeout', '60', '--artifacts', str(artifacts)] + flags
                row['started'] = time.time()
                log = (out / (row['name'] + '.log')).open('w')
                process = subprocess.Popen(row['argv'], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                                           start_new_session=True)
                active.append((process, log, row))
                meta['runs'].append(row)
                save()
                print('START', row['name'], 'expected', row['expected_comparisons'], flush=True)
            for process, log, row in list(active):
                status = process.poll()
                if status is None:
                    continue
                log.close()
                row.update(exit_status=status, finished=time.time())
                summary_path = out / row['name'] / 'summary.json'
                row['summary'] = json.loads(summary_path.read_text()) if summary_path.exists() else {'aborted': True}
                result = row['summary']
                row['complete'] = (status in (0, 1) and not result.get('aborted', True)
                                   and result.get('checked') == row['expected_comparisons']
                                   and result.get('failures', 100000) < 100000)
                active.remove((process, log, row))
                save()
                print('END', row['name'], json.dumps({'status': status, 'complete': row['complete'],
                      'checked': result.get('checked'), 'failures': result.get('failures')}), flush=True)
            if active:
                time.sleep(0.2)
    finally:
        for process, log, row in active:
            stop(process)
            log.close()
        save()
    # Classification reads saved results only, after every comparison process ends.
    for row in meta['runs']:
        argv = ['python3', str(SCRATCH / 'classify-final-parity.py'), str(out / row['name'])]
        result = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
        row['classifier'] = {'status': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
        save()
    meta['actual_physical_comparisons'] = sum(r['summary'].get('checked', 0) for r in meta['runs'])
    meta['finished'] = time.time()
    meta['total_raw_failures'] = sum(r['summary'].get('failures', 0) for r in meta['runs'])
    meta['complete'] = len(meta['runs']) == len(plans) and all(r['complete'] for r in meta['runs'])
    meta['coverage_complete'] = meta['complete']
    # These non-exhaustive runs retain the runner's full_parity=false contract.
    meta['full_parity'] = meta['complete'] and all(r['summary'].get('full_parity', False)
                                                 for r in meta['runs'])
    meta['classification_complete'] = all(r['classifier']['status'] == 0 for r in meta['runs'])
    save()
    assert meta['complete'], 'incomplete shard; preserve raw results and recover before claiming completion'
    assert meta['actual_physical_comparisons'] == meta['expected_physical_comparisons']
    assert meta['classification_complete'], 'classification failed; raw runner results remain unchanged'
    print(json.dumps({k: meta[k] for k in ['coverage_complete', 'full_parity', 'total_raw_failures',
                                          'configured_invocation_workloads', 'configured_invocation_comparisons',
                                          'configured_unique_workloads', 'configured_unique_comparison_variants',
                                          'actual_physical_comparisons', 'unique_workloads',
                                          'unique_comparison_variants', 'repeated_comparison_variants']}))


if __name__ == '__main__':
    main()
