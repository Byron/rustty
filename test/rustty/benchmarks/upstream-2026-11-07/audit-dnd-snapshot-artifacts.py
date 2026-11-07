"""Audit retained replies only; never start an oracle or change fixtures."""
from pathlib import Path
import hashlib
import json
import subprocess

ROOT = Path('/Users/byron/dev/github.com/ghostty-org/ghostty')
S = Path('/private/tmp/rustty-reintegrate.leadij')
CURRENT = S / 'final-dnd-snapshot-native-full/failures'
BASELINE = S / 'final-dnd-snapshot-before-linked/failures'
sha = lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest()

def differences(left, right, path='response'):
    if type(left) is not type(right):
        return [path + ':type']
    if isinstance(left, dict):
        if left.keys() != right.keys():
            return [path + ':keys']
        return [p for key in left for p in differences(left[key], right[key], path + '.' + key)]
    if isinstance(left, list):
        if len(left) != len(right):
            return [path + ':length']
        return [p for i, (a, b) in enumerate(zip(left, right))
                for p in differences(a, b, f'{path}[{i}]')]
    return [] if left == right else [path]

runner = ROOT / 'test/rustty/parity-runner.rs'
assert 'difference(&values[0], &values[1], "response", &["capabilities"])' in runner.read_text()
rust_error = b'EPERM:drop data can only be requested after a drop'
native_error = b'ENOENT:no drop data available'
records = []
for directory in sorted(CURRENT.iterdir()):
    if not directory.is_dir():
        continue
    baseline = BASELINE / directory.name
    request = json.loads((directory / 'request.json').read_text())
    assert request['kind'] == 'input' and 'expected_error' not in request
    assert request == json.loads((baseline / 'request.json').read_text())
    rust = json.loads((directory / 'rust.json').read_text())
    old = json.loads((baseline / 'rust.json').read_text())
    native = json.loads((directory / 'zig.json').read_text())
    assert rust['ok'] and native['ok'] and rust == old
    left = {k: v for k, v in rust.items() if k != 'capabilities'}
    right = {k: v for k, v in native.items() if k != 'capabilities'}
    paths = differences(left, right)
    assert len(paths) == 1 and paths[0] in ['response.events[1].data', 'response.events[3].data']
    index = int(paths[0].split('[')[1].split(']')[0])
    rust_bytes = bytes.fromhex(rust['events'][index]['data'])
    native_bytes = bytes.fromhex(native['events'][index]['data'])
    assert rust_bytes.count(rust_error) == 1
    assert rust_bytes.replace(rust_error, native_error) == native_bytes
    records.append({'id': request['id'], 'request_sha256': sha(directory / 'request.json'),
                    'difference_paths': paths, 'only_exact_error_message_differs': True,
                    'current_rust_equals_pre_link_raw_reply': True,
                    'current_rust_sha256': sha(directory / 'rust.json'),
                    'pre_link_rust_sha256': sha(baseline / 'rust.json'),
                    'native_sha256': sha(directory / 'zig.json')})
assert len(records) == 27
git = lambda *args: subprocess.check_output(['git', *args], cwd=ROOT)
initial_rust = git('show', 'c06197283:crates/rustty-vt/src/dnd.rs')
current_rust = git('show', 'f2b7e134:crates/rustty-vt/src/dnd.rs')
assert initial_rust == current_rust
native_before = git('show', '6ba144158:src/terminal/kitty/dnd_drop.zig')
assert native_error.split(b':', 1)[1] in native_before
incoming = git('diff', '6ba144158', '5acdbf154', '--', 'src/terminal/kitty/dnd_drop.zig')
(S / 'final-dnd-incoming.diff').write_bytes(incoming)
result = {'status': 'older intentional lifecycle difference; no classifier relaxation',
          'comparisons': len(records), 'standard_runner_root_exclusions': ['capabilities'],
          'runtime_driver': 'Rust parity-runner; Python here only audits retained JSON replies',
          'fixture_sha256': sha(S / 'final-dnd-snapshot-fixtures.json'),
          'runner_source_sha256': sha(runner), 'audit_source_sha256': sha(__file__),
          'binaries': {str(p): sha(p) for p in [S / 'linked-before-parity',
                      ROOT / 'target/release/examples/parity',
                      ROOT / 'target/release/examples/parity-runner',
                      ROOT / 'zig-out/bin/vt-oracle']},
          'rust_source_unchanged_since_pre_task': True,
          'rust_dnd_source_sha256': hashlib.sha256(current_rust).hexdigest(),
          'rust_policy_origin': 'fb662cab004cecd1c983ae95dc540dd8e3091aa6',
          'native_policy_origin': '50f69b883c',
          'upstream_base_has_same_no_data_error': True,
          'only_incoming_dnd_change': '6573de67e replaces ArrayListUnmanaged aliases only',
          'incoming_diff_sha256': sha(S / 'final-dnd-incoming.diff'),
          'records': records}
(S / 'final-dnd-snapshot-attribution.json').write_text(json.dumps(result, indent=2) + '\n')
print('27 complete failures: exact error-message-only difference; current Rust equals frozen pre-link Rust')
