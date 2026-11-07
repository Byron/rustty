from pathlib import Path
import json, hashlib
scratch = Path('/private/tmp/rustty-reintegrate.leadij')
prefix_dir = scratch / 'erase-prefixes'
prefix_dir.mkdir(exist_ok=True)
manifest = []
for entry in json.loads((scratch / 'ed-before-shared-selfchecks.json').read_text()):
    name = entry['name']
    source = scratch / 'upstream-corpora' / f'{name}.vt'
    full = source.read_bytes()
    data = full[:1024 * 1024]
    data = data[:data.rfind(b'\r\n') + 2]
    assert data and data.endswith(b'\r\n')
    target = prefix_dir / f'{name}.vt'
    target.write_bytes(data)
    config = json.loads((scratch / 'stream-configs' / f'{name}.json').read_text())
    config['data'] = str(target)
    (scratch / 'stream-configs' / f'ed-prefix-{name}.json').write_text(json.dumps(config, indent=2) + '\n')
    manifest.append({
        'name': name, 'parent': str(source),
        'parent_sha256': hashlib.sha256(full).hexdigest(),
        'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
        'counts': {'ED2': data.count(b'\x1b[2J'), 'ED22': data.count(b'\x1b[22J'),
                   'EL2': data.count(b'\x1b[2K'), 'EL0': data.count(b'\x1b[K'),
                   'CRLF': data.count(b'\r\n')},
    })
(prefix_dir / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
for row in manifest:
    print(row['name'], row['bytes'], row['counts'])
