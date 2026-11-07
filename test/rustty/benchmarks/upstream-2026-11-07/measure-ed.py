from pathlib import Path
import subprocess, json, time, hashlib
scratch = Path('/private/tmp/rustty-reintegrate.leadij')
cases = [('plain', 'plain.json', 100),
         ('erase-2-120x80', 'ed-prefix-erase-2-120x80.json', 2),
         ('erase-2-120x12', 'ed-prefix-erase-2-120x12.json', 8),
         ('erase-2-300x100', 'ed-prefix-erase-2-300x100.json', 1),
         ('erase-22-120x80', 'ed-prefix-erase-22-120x80.json', 8),
         ('erase-rate-120x80', 'ed-prefix-erase-rate-120x80.json', 4)]
meta = {'started': time.time(), 'before': '52a8151 / uxpryuy',
        'compiler': subprocess.check_output(['rustc', '+1.95.0', '-vV'], text=True),
        'profile': 'release, thin LTO, codegen-units=1, pinned offline lockfile',
        'binary_sha256': {name: hashlib.sha256((scratch / name).read_bytes()).hexdigest()
                          for name in ['ed-before-stream', 'ed-after-stream', 'sgr-native-primitives',
                                       'ed-before-controlled', 'ed-after-controlled']}}
results = []
for round in range(1, 4):
    for name, filename, iterations in cases:
        config = str(scratch / 'stream-configs' / filename)
        for version in (['before', 'after', 'native'] if round % 2 else ['native', 'after', 'before']):
            argv = ([str(scratch / 'sgr-native-primitives'), 'stream_file', config, str(iterations)]
                    if version == 'native' else
                    [str(scratch / f'ed-{version}-stream'), config, str(iterations)])
            result = subprocess.run(argv, capture_output=True, text=True)
            assert result.returncode == 0, (name, version, result.stderr)
            results.append({'round': round, 'name': name, 'version': version, 'argv': argv,
                            'status': result.returncode, 'result': json.loads(result.stdout),
                            'stderr': result.stderr})
            (scratch / 'ed-shared-timings.json').write_text(json.dumps(results, indent=2) + '\n')
        print(f'round {round}: {name} complete', flush=True)
controlled = []
for case in ['plain', 'erase-2', 'erase-2-seen', 'erase-22']:
    iterations = 20000 if case == 'plain' else 400
    for version in ['before', 'after']:
        argv = [str(scratch / f'ed-{version}-controlled'), case, str(iterations), '80']
        result = subprocess.run(argv, capture_output=True, text=True)
        assert result.returncode == 0, (case, version, result.stderr)
        controlled.append({'name': case, 'version': version, 'argv': argv,
                           'results': [json.loads(line) for line in result.stdout.splitlines()],
                           'status': result.returncode, 'stderr': result.stderr})
    (scratch / 'ed-controlled-timings.json').write_text(json.dumps(controlled, indent=2) + '\n')
    print(f'controlled {case} complete', flush=True)
meta['finished'] = time.time()
(scratch / 'ed-measurement-run.json').write_text(json.dumps(meta, indent=2) + '\n')
