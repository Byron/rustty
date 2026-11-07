from pathlib import Path
import hashlib, json, os, subprocess, time

root = Path('/Users/byron/dev/github.com/ghostty-org/ghostty')
scratch = Path('/private/tmp/rustty-reintegrate.leadij')
commands = [
    ('vt-lib', ['cargo', '+1.95.0', 'check', '--locked', '--offline', '-p', 'rustty-vt', '--lib', '--target', 'x86_64-pc-windows-msvc']),
    ('app-render-all-targets', ['cargo', '+1.95.0', 'check', '--locked', '--offline', '-p', 'rustty-app', '-p', 'rustty-render', '--all-targets', '--target', 'x86_64-pc-windows-msvc']),
]
record = {
    'scope': 'Windows MSVC compile checks only; no Windows runtime available',
    'cwd': str(root),
    'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
    'compiler': subprocess.check_output(['rustc', '+1.95.0', '-vV'], cwd=root, text=True),
    'environment_overrides': {},
    'relevant_inherited_environment': {name: os.environ.get(name) for name in [
        'CC', 'TARGET_CC', 'CC_x86_64_pc_windows_msvc', 'CC_x86_64-pc-windows-msvc',
        'CFLAGS', 'TARGET_CFLAGS', 'CFLAGS_x86_64_pc_windows_msvc',
        'CFLAGS_x86_64-pc-windows-msvc', 'VCINSTALLDIR', 'INCLUDE',
        'CARGO_TARGET_DIR', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS']},
    'all_targets_limit': 'The original combined VT/app/render all-targets command failed in Criterion 0.8.2 -> alloca 0.4.0 C compilation: Windows malloc.h unavailable. No prepared real MSVC C SDK setup was located; no SDK install, fabricated header, source edit, dependency substitution, or compiler macro workaround was used.',
    'preserved_failure_log': 'final-windows-check.log',
    'preserved_failure_log_sha256': hashlib.sha256((scratch / 'final-windows-check.log').read_bytes()).hexdigest(),
    'runs': [],
}
for name, argv in commands:
    log = scratch / ('final-windows-' + name + '.log')
    row = {'name': name, 'argv': argv, 'log': log.name, 'started': time.time()}
    with log.open('w') as output:
        result = subprocess.run(argv, cwd=root, stdout=output, stderr=subprocess.STDOUT)
    row.update(finished=time.time(), exit_status=result.returncode,
               log_sha256=hashlib.sha256(log.read_bytes()).hexdigest())
    record['runs'].append(row)
    (scratch / 'final-windows-validation.json').write_text(json.dumps(record, indent=2) + '\n')
    print(name, 'exit', result.returncode, flush=True)
record['supported_checks_passed'] = all(r['exit_status'] == 0 for r in record['runs'])
record['vt_all_targets_passed'] = False
record['runtime_tested'] = False
(scratch / 'final-windows-validation.json').write_text(json.dumps(record, indent=2) + '\n')
assert record['supported_checks_passed'], 'supported Windows compile check failed; inspect preserved logs'
