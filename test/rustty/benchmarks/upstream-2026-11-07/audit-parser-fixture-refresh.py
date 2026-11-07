#!/usr/bin/env python3
"""Require the observer QA refresh to change only the existing OSC-cancel case."""
import argparse
import copy
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location('fixture_audit', ROOT / 'audit-fixture-refresh.py')
BASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BASE)
CASE = 'parser/osc-cancel'
SOURCE = 'test/rustty/parity_reference.py'
OLD_DATA = '1b5d323b756e66696e6973686564187461696c1b5d323b6f746865721a'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_group(group, before, after, expected):
    errors = []
    if list(before['cases']) != list(after['cases']):
        errors.append('case IDs or ordering changed')
    if before['coverage'] != after['coverage']:
        errors.append('coverage counts changed')
    old_header, new_header = before['header'], after['header']
    if {k: v for k, v in old_header.items() if k != 'inputs'} != {
            k: v for k, v in new_header.items() if k != 'inputs'}:
        errors.append('header structure, counts, directory membership or native probes changed')
    old_inputs, new_inputs = old_header['inputs'], new_header['inputs']
    changed_inputs = sorted(k for k in old_inputs.keys() | new_inputs.keys()
                            if old_inputs.get(k) != new_inputs.get(k))
    if old_inputs.keys() != new_inputs.keys() or changed_inputs != [SOURCE]:
        errors.append('source membership or unexpected source CRC changed')
    changed_cases = []
    for name in before['cases'].keys() & after['cases'].keys():
        a, b = before['cases'][name], after['cases'][name]
        if a != b:
            changed_cases.append(name)
            if (group != 'parser' or name != CASE or a['covers'] != b['covers']
                    or a['comparisons'] != b['comparisons']
                    or b['request_sha256'] != BASE.digest(expected['request'])):
                errors.append(f'unexpected case change: {name}')
    if sorted(changed_cases) != ([CASE] if group == 'parser' else []):
        errors.append('expected exactly one parser/osc-cancel request change across all groups')
    if before['dnd'] != after['dnd']:
        errors.append('historical or current DND request/coverage changed')
    return {'changed_cases': sorted(changed_cases), 'changed_source_inputs': changed_inputs,
            'cases': len(after['cases']), 'comparisons': after['manifest']['comparisons'],
            'errors': errors}


def audit(args):
    provenance = json.loads(args.before_provenance.read_text())
    for name, record in provenance['files'].items():
        path = args.before / name
        if path.stat().st_size != record['bytes'] or sha(path) != record['sha256']:
            raise ValueError(f'first-final copy differs from recorded immutable source: {name}')
    expected_cases = json.loads(args.expected_case.read_text())
    if len(expected_cases) != 1 or expected_cases[0]['request']['id'] != CASE:
        raise ValueError('expected-case must contain exactly the reviewed parser/osc-cancel record')
    expected = expected_cases[0]
    before_manifest = json.loads((args.before / 'manifest.json').read_text())
    after_manifest = json.loads((args.after / 'manifest.json').read_text())
    expected_oracles = {'rust': sha(args.rust_bin), 'zig': sha(args.zig_bin)}
    result = {'status': 'pending', 'scope': 'Strict saved-archive/source audit; no oracle or runtime-parity claim.',
              'before_provenance_sha256': sha(args.before_provenance),
              'before_commit': provenance['source_commit'], 'after_commit': args.after_commit,
              'expected_case_sha256': sha(args.expected_case), 'oracles': expected_oracles,
              'groups': {}, 'errors': []}
    for label, directory, manifest in [('before', args.before, before_manifest), ('after', args.after, after_manifest)]:
        if (set(manifest) != set(BASE.GROUPS) or
                {p.name.removesuffix('.jsonl.gz') for p in directory.glob('*.jsonl.gz')} != set(BASE.GROUPS)):
            raise ValueError(f'{label}: expected exactly all 12 archive/manifest groups')
    for group in BASE.GROUPS:
        before = BASE.read_group(args.before, group, before_manifest)
        after = BASE.read_group(args.after, group, after_manifest)
        row = check_group(group, before, after, expected)
        row['errors'] += BASE.source_errors(after['header'], args.repo_root.resolve())
        with gzip.open(args.after / f'{group}.jsonl.gz', 'rt') as stream:
            next(stream)
            if any(set(json.loads(line)) != {'request', 'covers'} for line in stream):
                row['errors'].append('unexpected fixture record fields')
        old_meta, new_meta = before_manifest[group], after_manifest[group]
        if new_meta['commit'] != args.after_commit or new_meta['oracles'] != expected_oracles:
            row['errors'].append('regeneration commit or oracle hashes differ from explicit expected values')
        if {k: v for k, v in old_meta.items() if k not in ('commit', 'oracles', 'sha256', 'bytes')} != {
                k: v for k, v in new_meta.items() if k not in ('commit', 'oracles', 'sha256', 'bytes')}:
            row['errors'].append('manifest schema or case/comparison counts changed')
        row['archive_sha256'] = {'before': old_meta['sha256'], 'after': new_meta['sha256']}
        if group == 'parser':
            records = []
            for directory in (args.before, args.after):
                with gzip.open(directory / 'parser.jsonl.gz', 'rt') as stream:
                    next(stream)
                    records.append(next(record for line in stream
                                        if (record := json.loads(line))['request']['id'] == CASE))
            original = copy.deepcopy(expected)
            original['request']['operations'][0]['data'] = OLD_DATA
            if records != [original, expected]:
                row['errors'].append('old/new OSC-cancel records differ from the two reviewed exact requests')
        if group == 'protocols':
            ids = {BASE.DND + f'{engine}/{i}' for engine in ('rust', 'rust-current', 'zig') for i in range(3)}
            if set(before['dnd']) != ids or set(after['dnd']) != ids:
                row['errors'].append('expected all nine DND snapshot IDs')
        result['groups'][group] = row
        result['errors'] += [f'{group}: {error}' for error in row['errors']]
    legacy = 'dnd-snapshot-rust-v1.json'
    if (args.before / legacy).read_bytes() != (args.after / legacy).read_bytes():
        result['errors'].append('historical Rust DND source bytes changed')
    result['status'] = 'invalid' if result['errors'] else 'preserved_with_exact_parser_regression'
    return result


def selfcheck():
    record = {'request': {'id': CASE, 'kind': 'parser', 'operations': []}, 'covers': ['parser.raw-events']}
    case = {'request_sha256': 'old', 'covers': record['covers'], 'comparisons': 3}
    old = {'cases': {CASE: case}, 'coverage': {'parser.raw-events': 1}, 'manifest': {'comparisons': 3},
           'header': {'inputs': {SOURCE: 1}, 'directories': {}, 'probes': [], 'cases': 1, 'format': 1}, 'dnd': {}}
    new = copy.deepcopy(old)
    new['header']['inputs'][SOURCE] = 2
    new['cases'][CASE]['request_sha256'] = BASE.digest(record['request'])
    assert not check_group('parser', old, new, record)['errors']
    for mutate in (
        lambda x: x['cases'].clear(),
        lambda x: x['cases'][CASE]['covers'].append('changed'),
        lambda x: x['header']['probes'].append({'changed': True}),
        lambda x: x['header']['directories'].update({'corpus': ['changed']}),
        lambda x: x['header']['inputs'].update({'unexpected': 3}),
        lambda x: x['dnd'].update({'lost-history': {}}),
    ):
        bad = copy.deepcopy(new)
        mutate(bad)
        assert check_group('parser', old, bad, record)['errors']
    assert check_group('parser', old, old, record)['errors']
    assert check_group('grid', old, new, record)['errors']
    print('Parser-only fixture audit selfcheck passed; no real archives or oracles executed.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selfcheck', action='store_true')
    for name in ('before', 'after', 'before-provenance', 'expected-case', 'repo-root', 'rust-bin', 'zig-bin', 'output'):
        parser.add_argument('--' + name, type=Path)
    parser.add_argument('--after-commit')
    args = parser.parse_args()
    if args.selfcheck:
        selfcheck()
    else:
        if any(value is None for name, value in vars(args).items() if name != 'selfcheck'):
            parser.error('all path arguments and --after-commit are required')
        result = audit(args)
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
        print(json.dumps({'status': result['status'], 'errors': result['errors']}))
        raise SystemExit(bool(result['errors']))
