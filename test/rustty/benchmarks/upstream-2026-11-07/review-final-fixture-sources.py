#!/usr/bin/env python3
"""Reproduce fixture source transformations using saved replies, never oracles."""
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import types
import zlib

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent
REPO = Path('/Users/byron/dev/github.com/ghostty-org/ghostty')


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def archive(directory, group):
    with gzip.open(directory / (group + '.jsonl.gz'), 'rt') as stream:
        header = json.loads(next(stream))
        records = [json.loads(line) for line in stream]
    return header, {record['request']['id']: record for record in records}


class SavedReplies:
    def __init__(self, header):
        self.replies = {}
        self.used = set()
        for probe in header['probes']:
            key = digest(probe['request'])
            if key in self.replies:
                assert self.replies[key] == probe['response']
            self.replies[key] = probe['response']

    def request(self, request):
        key = digest(request)
        self.used.add(key)
        return self.replies[key]


audit = json.loads((ROOT / 'final-fixture-audit.json').read_text())
result = {'scope': 'Source transformation audit only; recorded native replies, no oracle or runtime comparison.',
          'audit_sha256': hashlib.sha256((ROOT / 'final-fixture-audit.json').read_bytes()).hexdigest(),
          'sides': {}}
generated_ids = {}
for side, directory in [('before', ROOT / 'fixtures-before-refresh'), ('after', REPO / 'test/rustty/fixtures')]:
    groups = {group: archive(directory, group) for group in ('pages', 'page-layout', 'snapshot-wire', 'snapshots', 'osc')}
    commit = audit['groups']['pages'][side]['commit']
    modules = {}
    sources = {}
    for name in ('snapshots', 'snapshot_resources', 'page_lifecycle_requests', 'style_lifecycle_requests', 'page_layout_requests', 'osc_requests'):
        path = 'test/rustty/' + name + '.py'
        raw = subprocess.check_output(['git', 'show', commit + ':' + path], cwd=REPO)
        expected_crc = groups['pages'][0]['inputs'].get(path)
        if expected_crc is not None:
            assert zlib.crc32(raw) == expected_crc, (side, path)
        module = types.ModuleType(name)
        module.__file__ = str(REPO / path)
        sys.modules[name] = modules[name] = module
        exec(compile(raw, path, 'exec'), module.__dict__)
        sources[path] = hashlib.sha256(raw).hexdigest()
    sections = []
    for group, module_name, function, needs_replies in [
        ('pages', 'page_lifecycle_requests', 'requests', True),
        ('pages', 'style_lifecycle_requests', 'requests', True),
        ('page-layout', 'page_layout_requests', 'requests', False),
        ('snapshot-wire', 'snapshot_resources', 'style_requests', True),
        ('snapshots', 'snapshots', 'requests', False),
    ]:
        header, recorded = groups[group]
        replies = SavedReplies(header)
        generated = getattr(modules[module_name], function)(replies) if needs_replies else getattr(modules[module_name], function)()
        checked = []
        for request, covers in generated:
            case_id = request['id']
            assert recorded[case_id] == {'request': request, 'covers': covers}, (side, group, case_id)
            checked.append(case_id)
        generated_ids.setdefault(group, set()).update(checked)
        sections.append({'group': group, 'source': module_name + '.' + function,
                         'exact_records': len(checked), 'case_ids': checked,
                         'saved_probe_requests_used': sorted(replies.used)})
    # Check both changed OSC seeds against their exact Git source bytes.
    osc_checked = []
    for change in audit['groups']['osc']['changed_requests']:
        case_id = change[side + '_id']
        suffix = case_id.removeprefix('osc/corpus/')
        path = 'test/fuzz-libghostty/corpus/' + suffix
        raw = subprocess.check_output(['git', 'show', commit + ':' + path], cwd=REPO)
        request = groups['osc'][1][case_id]['request']
        assert bytes.fromhex(request['operations'][1]['data']) == raw
        osc_checked.append({'id': case_id, 'source': path, 'source_sha256': hashlib.sha256(raw).hexdigest()})
    generated_ids.setdefault('osc', set()).update(x['id'] for x in osc_checked)
    result['sides'][side] = {'commit': commit, 'source_sha256': sources, 'sections': sections, 'osc': osc_checked}

uncovered = {group: [x['after_id'] for x in value['changed_requests'] if x['after_id'] not in generated_ids.get(group, set())]
             for group, value in audit['groups'].items()}
assert not any(uncovered.values()), uncovered
result['all_changed_requests_reproduced'] = sum(len(g['changed_requests']) for g in audit['groups'].values())
result['status'] = 'All changed requests exactly reproduced from their recorded source checkpoint and saved native replies.'
(ROOT / 'final-fixture-source-review.json').write_text(json.dumps(result, indent=2) + '\n')
print(result['status'], result['all_changed_requests_reproduced'])
