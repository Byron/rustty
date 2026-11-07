"""Audit saved protocol failures and pre-task source; never run an oracle."""
from collections import Counter
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
ROOT = Path('/private/tmp/rustty-reintegrate.leadij')
REPO = Path('/Users/byron/dev/github.com/ghostty-org/ghostty')
START = 'c06197283e8d24f623c981b428ca8b3e56ad8f9a'
CURRENT = '03be726e243bdc235bd641420f511fc2b7792f8e'
spec = importlib.util.spec_from_file_location('classifier', ROOT / 'classify-final-parity.py')
classifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(classifier)
sha = lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
git = lambda *args: subprocess.check_output(['git', *args], cwd=REPO)


def cases(path):
    result = {}
    for directory in path.iterdir():
        names = ('request.json', 'rust.json', 'zig.json', 'difference.txt')
        if not all((directory / name).is_file() for name in names):
            continue
        request = json.loads((directory / 'request.json').read_text())
        result[request['id']] = {'directory': directory, 'request': request,
                                'rust': json.loads((directory / 'rust.json').read_text()),
                                'zig': json.loads((directory / 'zig.json').read_text())}
    return result


current = cases(ROOT / 'final-parity/groups-1/failures')
baseline = cases(ROOT / 'final-protocol-residual-before-carry/failures')
summary = json.loads((ROOT / 'final-protocol-residual-before-carry/summary.json').read_text())
assert summary['checked'] == summary['failures'] == 177
assert not summary['aborted'] and summary['error'] is None
records = []
counts = Counter()
for name, old in sorted(baseline.items()):
    new = current[name]
    assert old['request'] == new['request'], name
    assert old['rust'] == new['rust'], name
    assert old['zig'] == new['zig'], name
    assert sha(old['directory'] / 'rust.json') == sha(new['directory'] / 'rust.json'), name
    assert sha(old['directory'] / 'zig.json') == sha(new['directory'] / 'zig.json'), name
    classification = classifier.classify(new['zig'], new['rust'])
    family = '/'.join(name.split('/')[:3])
    counts[(family, classification['classification'])] += 1
    paths = list(classifier.differences(new['zig'], new['rust']))
    records.append({'id': name, 'family': family,
                    'current_directory': str(new['directory'].relative_to(ROOT)),
                    'baseline_directory': str(old['directory'].relative_to(ROOT)),
                    'request_sha256': sha(new['directory'] / 'request.json'),
                    'current_rust_sha256': sha(new['directory'] / 'rust.json'),
                    'baseline_rust_sha256': sha(old['directory'] / 'rust.json'),
                    'native_sha256': sha(new['directory'] / 'zig.json'),
                    'current_rust_equals_baseline_raw_reply': True,
                    'current_native_equals_baseline_raw_reply': True,
                    'all_difference_paths': paths, 'classification': classification})
assert len(records) == 177
assert sum(n for (_, c), n in counts.items() if c == 'existing-pointer-shape-record-only') == 69
assert sha(ROOT / 'carry-before-parity') == json.loads((ROOT / 'carry-before-build.json').read_text())['binaries']['carry-before-parity']

sources = []
for path in ('crates/rustty-vt/src/dnd.rs', 'test/rustty/rust-dnd.rs',
             'src/terminal/device_status.zig', 'src/terminal/osc/parsers/osc9.zig'):
    old, new = git('show', START + ':' + path), git('show', CURRENT + ':' + path)
    assert old == new, path
    sources.append({'path': path, 'start_equals_current': True,
                    'sha256': hashlib.sha256(old).hexdigest()})
old_terminal = git('show', START + ':crates/rustty-vt/src/terminal.rs').decode()
new_terminal = git('show', CURRENT + ':crates/rustty-vt/src/terminal.rs').decode()
regions = []
for label, begin, end in (
    ('OSC 9 handler', '    fn osc9(', None),
    ('unsigned parser', 'fn parse_unsigned(', '\nfn sgr_report('),
    ('DSR dispatch', "            ([], b'n') | ([b'?'], b'n')", "            ([], b'c')"),
):
    values = []
    for text in (old_terminal, new_terminal):
        first = text.index(begin)
        last = (text.index(end, first) if end else
                first + re.search(r'\n    fn ', text[first + len(begin):]).start() + len(begin))
        values.append(text[first:last])
    assert values[0] == values[1], label
    regions.append({'region': label, 'start_equals_current': True,
                    'sha256': hashlib.sha256(values[0].encode()).hexdigest()})
native_origins = []
for change, reason in (
    ('73768913b47820aae4f077ecbddec111042661c3', 'OSC digit-separator rejection'),
    ('fbda61b873da5208475c183af160d131ac7a77a3', 'DECXCPR extended cursor-position response'),
):
    assert subprocess.run(['git', 'merge-base', '--is-ancestor', change, '6ba144158'], cwd=REPO).returncode == 0
    native_origins.append({'commit': change, 'behavior': reason, 'ancestor_of_old_upstream_base': True})
native_dnd = git('diff', '6ba144158', '5acdbf154', '--', 'src/terminal/kitty/dnd_drop.zig')
assert all('std.ArrayList' in line or line.startswith(('---', '+++'))
           for line in native_dnd.decode().splitlines() if line.startswith(('-', '+')))

result = {
    'status': 'All 177 raw failures reproduce on the frozen pre-carry Rust binary; no incoming protocol regression found in this category.',
    'scope': 'Saved full-field replies and immutable source comparison. Fresh baseline used Rust parity-runner; Python only orchestrates/audits.',
    'current_commit': CURRENT, 'pre_task_commit': START,
    'baseline_commit': json.loads((ROOT / 'carry-before-build.json').read_text())['commit'],
    'baseline_summary': summary,
    'baseline_argv': ['target/release/examples/parity-runner', '--no-build', '--rust-bin', str(ROOT / 'carry-before-parity'),
                      '--zig-bin', 'zig-out/bin/vt-oracle', '--fixtures', str(ROOT / 'final-protocol-residual-fixtures.json'),
                      '--artifacts', str(ROOT / 'final-protocol-residual-before-carry'), '--max-failures', '1000'],
    'baseline_exit_code': 1,
    'binary_sha256': {str(path): sha(path) for path in (ROOT / 'carry-before-parity', REPO / 'target/release/examples/parity', REPO / 'target/release/examples/parity-runner', REPO / 'zig-out/bin/vt-oracle')},
    'supporting_sha256': {name: sha(ROOT / name) for name in ('carry-before-build.json', 'final-protocol-residual-fixtures.json', 'final-protocol-residual-before-carry.log', 'final-protocol-residual-before-carry/summary.json', 'classify-final-parity.py', 'audit-final-protocol-residuals.py')},
    'counts': [{'family': family, 'classification': classification, 'comparisons': n} for (family, classification), n in sorted(counts.items())],
    'source_equivalence': sources, 'terminal_regions': regions, 'native_origins': native_origins,
    'dnd_origin': 'fb662cab004cecd1c983ae95dc540dd8e3091aa6 (November 5 native drag/drop lifecycle refinement), with prior base implementation 74f3e4935feb6713286c0570f289a8f3b24a2e4c',
    'native_dnd_incoming': 'Only two ArrayListUnmanaged aliases become ArrayList; no behavioral changes.',
    'pointer_shape_origin': 'b438193fb9609e41de4aa6f0302efd3e3eeb3b36 precedes the task; exact common frames/fields agree in all 69 classified replies.',
    'records': records,
    'limits': ['All 177 remain raw comparison failures; diagnostic classification is not a passed runner.',
               'The 69 pointer-only records do not validate snapshot CRCs; the classifier compares stored CRC bytes and exact remaining frames.',
               'The other 108 differences remain substantive older DND/progress/DSR differences; none is silently excluded.'],
}
(ROOT / 'final-protocol-residual-attribution.json').write_text(json.dumps(result, indent=2) + '\n')
print('177 exact baseline replies; 69 pointer-record-only, 108 substantive older differences; source ancestry verified')
