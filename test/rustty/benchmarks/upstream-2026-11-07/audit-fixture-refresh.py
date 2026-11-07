#!/usr/bin/env python3
"""Audit regenerated frozen fixtures without running generators or oracles."""
import argparse
from collections import Counter, defaultdict
import gzip
import hashlib
import json
from pathlib import Path
import tempfile
import zlib

GROUPS = ('input', 'parser', 'grid', 'page-layout', 'pages', 'thorough',
          'unicode', 'snapshots', 'snapshot-wire', 'protocols', 'corpus', 'osc')
DND = 'protocol/dnd/snapshot/'
LEGACY_SOURCE = 'test/rustty/fixtures/dnd-snapshot-rust-v1.json'


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def comparisons(request):
    return 1 if request.get('expected_error') or request.get('kind', 'terminal') not in (
        'terminal', 'input', 'parser', 'snapshot') else 3


def read_group(directory, group, manifest):
    path = directory / f'{group}.jsonl.gz'
    raw = path.read_bytes()
    meta = manifest[group]
    if len(raw) != meta['bytes'] or hashlib.sha256(raw).hexdigest() != meta['sha256']:
        raise ValueError(f'{group}: archive bytes/hash disagree with manifest')
    cases, dnd, coverage = {}, {}, Counter()
    with gzip.open(path, 'rt') as stream:
        header = json.loads(next(stream))
        if header['format'] != 1:
            raise ValueError(f'{group}: unsupported archive format')
        for line in stream:
            record = json.loads(line)
            request, covers = record['request'], record['covers']
            name = request['id']
            if not isinstance(name, str) or name in cases:
                raise ValueError(f'{group}: invalid or duplicate ID {name!r}')
            if not isinstance(covers, list) or not all(isinstance(tag, str) for tag in covers):
                raise ValueError(f'{group}/{name}: invalid coverage')
            cases[name] = {'request_sha256': digest(request), 'covers': covers,
                           'comparisons': comparisons(request)}
            coverage.update(set(covers))
            if name.startswith(DND):
                dnd[name] = record
    count = sum(case['comparisons'] for case in cases.values())
    if len(cases) != header['cases'] or len(cases) != meta['cases'] or count != meta['comparisons']:
        raise ValueError(f'{group}: case/comparison counts disagree with header or manifest')
    probes, probe_index = defaultdict(Counter), defaultdict(list)
    for index, probe in enumerate(header['probes']):
        key = digest(probe['request'])
        probes[key][digest(probe['response'])] += 1
        probe_index[key].append({'index': index, 'id': probe['request'].get('id')})
    return {'manifest': meta, 'header': header, 'cases': cases, 'coverage': coverage,
            'probes': dict(probes), 'probe_index': dict(probe_index), 'dnd': dnd}


def changes(before, after):
    return {'added': {key: after[key] for key in sorted(after.keys() - before.keys())},
            'removed': {key: before[key] for key in sorted(before.keys() - after.keys())},
            'changed': {key: {'before': before[key], 'after': after[key]}
                        for key in sorted(before.keys() & after.keys()) if before[key] != after[key]}}


def compare_group(before, after, renames):
    old, new = before['cases'], after['cases']
    removed, added = sorted(old.keys() - new.keys()), sorted(new.keys() - old.keys())
    paired = [(name, name) for name in sorted(old.keys() & new.keys())]
    rename_old, rename_new = set(), set()
    for rename in renames:
        left, right = rename['before'], rename['after']
        if (left not in removed or right not in added or left in rename_old or right in rename_new
                or not isinstance(rename.get('evidence'), str) or not rename['evidence'].strip()
                or old[left]['request_sha256'] != rename.get('before_request_sha256')
                or new[right]['request_sha256'] != rename.get('after_request_sha256')):
            raise ValueError(f'invalid or unsubstantiated rename: {rename!r}')
        rename_old.add(left)
        rename_new.add(right)
        paired.append((left, right))
    request_changes, cover_changes = [], []
    for left, right in paired:
        a, b = old[left], new[right]
        if a['request_sha256'] != b['request_sha256']:
            request_changes.append({'before_id': left, 'after_id': right,
                                    'before_sha256': a['request_sha256'], 'after_sha256': b['request_sha256']})
        lost = sorted(set(a['covers']) - set(b['covers']))
        gained = sorted(set(b['covers']) - set(a['covers']))
        if lost or gained:
            cover_changes.append({'before_id': left, 'after_id': right, 'removed': lost, 'added': gained})
    lost_coverage = {tag: {'before': count, 'after': after['coverage'].get(tag, 0)}
                     for tag, count in sorted(before['coverage'].items()) if after['coverage'].get(tag, 0) < count}
    probe_delta = changes(before['probes'], after['probes'])
    source_delta = changes(before['header']['inputs'], after['header']['inputs'])
    directory_delta = changes(before['header']['directories'], after['header']['directories'])
    directory_losses = {name: sorted(set(members) - set(after['header']['directories'].get(name, [])))
                        for name, members in before['header']['directories'].items()
                        if set(members) - set(after['header']['directories'].get(name, []))}
    findings = []
    for label, value in [('unexplained_removed_ids', set(removed) - rename_old),
                         ('coverage_count_decreases', lost_coverage),
                         ('per_case_coverage_losses', [x for x in cover_changes if x['removed']]),
                         ('changed_requests', request_changes),
                         ('removed_source_inputs', source_delta['removed']),
                         ('removed_source_directories', directory_delta['removed']),
                         ('source_directory_member_losses', directory_losses),
                         ('removed_native_probes', probe_delta['removed']),
                         ('changed_native_probe_responses', probe_delta['changed'])]:
        if value:
            findings.append({'kind': label, 'count': len(value)})
    return {'before': before['manifest'], 'after': after['manifest'], 'added_ids': added,
            'removed_ids': removed, 'recorded_renames': renames,
            'added_cases': {name: new[name] for name in added},
            'removed_cases': {name: old[name] for name in removed},
            'unexplained_removed_ids': sorted(set(removed) - rename_old),
            'changed_requests': request_changes,
            'coverage': {'before': before['coverage'], 'after': after['coverage'],
                         'count_decreases': lost_coverage, 'per_case_changes': cover_changes},
            'native_probes': {'before_count': sum(sum(x.values()) for x in before['probes'].values()),
                             'after_count': sum(sum(x.values()) for x in after['probes'].values()),
                             'before_index': before['probe_index'], 'after_index': after['probe_index'], **probe_delta},
            'sources': {'inputs': source_delta,
                        'directories': directory_delta, 'directory_member_losses': directory_losses},
            'review_findings': findings}


def source_errors(header, repo):
    errors = []
    for name, expected in header['inputs'].items():
        path = (repo / name).resolve()
        if not path.is_relative_to(repo) or not path.is_file():
            errors.append(f'missing/nonlocal source: {name}')
        elif zlib.crc32(path.read_bytes()) != expected:
            errors.append(f'stale source CRC: {name}')
    for name, expected in header['directories'].items():
        path = (repo / name).resolve()
        if not path.is_relative_to(repo) or not path.is_dir():
            errors.append(f'missing/nonlocal source directory: {name}')
        elif sorted(item.name for item in path.iterdir() if item.is_file()) != expected:
            errors.append(f'stale directory membership: {name}')
    return errors


def dnd_check(before, after, source):
    old, new = before['dnd'], after['dnd']
    expected = {DND + f'{engine}/{i}' for engine in ('rust', 'rust-current', 'zig') for i in range(3)}
    errors, records = [], []
    expected_before = {DND + f'{engine}/{i}' for engine in ('rust', 'zig') for i in range(3)}
    if set(old) != expected_before:
        errors.append({'kind': 'original_DND_ID_set', 'missing': sorted(expected_before - old.keys()),
                       'unexpected': sorted(old.keys() - expected_before)})
    if set(new) != expected:
        errors.append({'kind': 'DND_ID_set', 'missing': sorted(expected - new.keys()),
                       'unexpected': sorted(new.keys() - expected)})
    stored = {case['request']['id']: case for case in source['cases']}
    legacy_ids = {DND + f'rust/{i}' for i in range(3)}
    if set(stored) != legacy_ids or len(source['cases']) != 3:
        errors.append({'kind': 'legacy_source_ID_set'})
    for name in sorted(legacy_ids):
        same = name in old and old[name] == new.get(name) == stored.get(name)
        records.append({'id': name, 'exact_request_and_covers_preserved': same,
                        'before_record_sha256': digest(old[name]) if name in old else None,
                        'after_record_sha256': digest(new[name]) if name in new else None})
        if not same:
            errors.append({'kind': 'historical_DND_record_changed', 'id': name})
    for key, value in before['manifest'].items():
        if source['source'].get(key) != value:
            errors.append({'kind': 'legacy_source_provenance', 'field': key})
    if LEGACY_SOURCE not in after['header']['inputs']:
        errors.append({'kind': 'legacy_source_missing_from_generator_CRC_inputs'})
    return {'historical_records': records, 'fresh_rust_ids': sorted(x for x in new if '/rust-current/' in x),
            'zig_ids': sorted(x for x in new if '/zig/' in x), 'errors': errors}


def audit(args):
    before_manifest = json.loads((args.before / 'manifest.json').read_text())
    after_manifest = json.loads((args.after / 'manifest.json').read_text())
    renames = json.loads(args.renames.read_text()) if args.renames else []
    if not isinstance(renames, list) or any(r.get('group') not in GROUPS for r in renames):
        raise ValueError('rename records must be a list with known groups')
    result = {'status': 'pending', 'before_directory': str(args.before), 'after_directory': str(args.after),
              'repo_root': str(args.repo_root), 'groups': {}, 'errors': [],
              'note': 'Archive/source preservation audit only; no oracle execution or runtime parity claim. '
                      'Changed requests/probes and losses require explicit review; recorded renames never erase raw deltas.'}
    for label, directory, manifest in [('before', args.before, before_manifest), ('after', args.after, after_manifest)]:
        archive_groups = {p.name.removesuffix('.jsonl.gz') for p in directory.glob('*.jsonl.gz')}
        if set(manifest) != set(GROUPS) or archive_groups != set(GROUPS):
            result['errors'].append(f'{label}: expected exactly the 12 known archive/manifest groups')
    for group in GROUPS:
        try:
            before = read_group(args.before, group, before_manifest)
            after = read_group(args.after, group, after_manifest)
            row = compare_group(before, after, [r for r in renames if r['group'] == group])
            row['source_errors'] = source_errors(after['header'], args.repo_root)
            if row['source_errors']:
                result['errors'].append(f'{group}: current source provenance mismatch')
            result['groups'][group] = row
            if group == 'protocols':
                source = json.loads((args.repo_root / LEGACY_SOURCE).read_text())
                result['dnd'] = dnd_check(before, after, source)
                result['errors'].extend(result['dnd']['errors'])
        except (ValueError, KeyError, OSError, StopIteration) as error:
            result['errors'].append(f'{group}: {error}')
    provenance = {(row['commit'], tuple(sorted(row['oracles'].items()))) for row in after_manifest.values()}
    result['after_provenance_uniform'] = len(provenance) == 1
    if not result['after_provenance_uniform']:
        result['errors'].append('after groups have different source commits/oracle hashes; full refresh not established')
    findings = sum(len(row['review_findings']) for row in result['groups'].values())
    result['review_finding_count'] = findings
    result['status'] = 'invalid' if result['errors'] else 'review_required' if findings else 'preserved'
    return result


def selfcheck():
    def fake(cases, probes=None):
        return {'cases': cases, 'manifest': {}, 'coverage': Counter(tag for c in cases.values() for tag in set(c['covers'])),
                'probes': probes or {}, 'probe_index': {}, 'header': {'inputs': {}, 'directories': {}}, 'dnd': {}}
    a = {'request_sha256': 'a', 'covers': ['kept', 'lost'], 'comparisons': 3}
    b = {'request_sha256': 'b', 'covers': ['kept'], 'comparisons': 3}
    old, new = fake({'old': a}, {'probe': Counter({'old-response': 1})}), fake({'new': b}, {'probe': Counter({'new-response': 1})})
    assert compare_group(old, old, [])['review_findings'] == []
    assert compare_group(old, new, [])['unexplained_removed_ids'] == ['old']
    rename = {'group': 'pages', 'before': 'old', 'after': 'new', 'before_request_sha256': 'a',
              'after_request_sha256': 'b', 'evidence': 'Recorded capacity boundary changed; source line and ledgers reviewed.'}
    delta = compare_group(old, new, [rename])
    assert delta['removed_ids'] == ['old'] and delta['unexplained_removed_ids'] == []
    assert delta['coverage']['count_decreases'] == {'lost': {'before': 1, 'after': 0}}
    assert delta['native_probes']['changed'] and delta['changed_requests'] and delta['review_findings']
    try:
        compare_group(old, new, [{**rename, 'evidence': ''}])
        raise AssertionError('rename without evidence accepted')
    except ValueError:
        pass
    record = lambda name: {'request': {'id': name, 'ops': [{'op': 'restore', 'data': 'abcd'}]}, 'covers': ['snapshot']}
    old['dnd'] = {DND + f'{engine}/{i}': record(DND + f'{engine}/{i}') for engine in ('rust', 'zig') for i in range(3)}
    new['dnd'] = {DND + f'{engine}/{i}': record(DND + f'{engine}/{i}') for engine in ('rust', 'rust-current', 'zig') for i in range(3)}
    new['header']['inputs'][LEGACY_SOURCE] = 0
    source = {'source': {}, 'cases': [r for name, r in old['dnd'].items() if '/rust/' in name]}
    assert not dnd_check(old, new, source)['errors']
    new['dnd'][DND + 'rust/0']['request']['ops'][0]['data'] = 'abce'
    assert dnd_check(old, new, source)['errors']
    del new['dnd'][DND + 'rust-current/2']
    assert any(e['kind'] == 'DND_ID_set' for e in dnd_check(old, new, source)['errors'])
    with tempfile.TemporaryDirectory(prefix='fixture-audit-selfcheck-') as directory:
        root = Path(directory).resolve()
        (root / 'source.py').write_text('source\n')
        header = {'format': 1, 'cases': 1, 'inputs': {'source.py': zlib.crc32(b'source\n')}, 'directories': {}, 'probes': []}
        path = root / 'pages.jsonl.gz'
        records = [header, record('case')]
        path.write_bytes(gzip.compress(('\n'.join(json.dumps(r) for r in records) + '\n').encode(), mtime=0))
        manifest = {'pages': {'cases': 1, 'comparisons': 3, 'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}}
        assert len(read_group(root, 'pages', manifest)['cases']) == 1
        assert source_errors(header, root) == []
        (root / 'source.py').write_text('changed\n')
        assert source_errors(header, root)
        manifest['pages']['sha256'] = 'bad'
        try:
            read_group(root, 'pages', manifest)
            raise AssertionError('bad manifest accepted')
        except ValueError:
            pass
    print('Fixture preservation audit selfcheck passed; no repository archives or oracles read.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selfcheck', action='store_true')
    for name in ('before', 'after', 'repo-root', 'output', 'renames'):
        parser.add_argument('--' + name, type=Path)
    args = parser.parse_args()
    if args.selfcheck:
        selfcheck()
        return
    if any(getattr(args, name) is None for name in ('before', 'after', 'repo_root', 'output')):
        parser.error('--before, --after, --repo-root and --output are required')
    args.repo_root = args.repo_root.resolve()
    result = audit(args)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    print(f"{result['status']}: {len(result['groups'])} groups, {len(result['errors'])} errors, {result['review_finding_count']} review categories; {args.output}")
    raise SystemExit(0 if result['status'] == 'preserved' else 1)


if __name__ == '__main__':
    main()
