#!/usr/bin/env python3
"""Verify and package an explicitly finalized artifact selection and reviewed overlays."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import stat
import tempfile

MANIFEST = 'artifact-manifest.json'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read(root, name):
    relative = PurePosixPath(name)
    if (relative.is_absolute() or not relative.parts or relative.as_posix() != name
            or any(part in ('.', '..') for part in relative.parts)):
        raise ValueError(f'unsafe relative path: {name!r}')
    path = root.joinpath(*relative.parts)
    if not path.resolve().is_relative_to(root.resolve()) or not path.is_file():
        raise ValueError(f'missing or nonlocal file: {name!r}')
    return path.read_bytes(), stat.S_IMODE(path.stat().st_mode)


def package(selection_path, source_root, overlay_root, destination):
    if destination.exists() or destination.is_symlink():
        raise ValueError(f'destination already exists: {destination}')
    selection_bytes = selection_path.read_bytes()
    selection = json.loads(selection_bytes)
    if selection.get('finalized') is not True:
        raise ValueError('selection must explicitly contain "finalized": true; drafts are not packaged')
    names, hashes = selection['files'], selection['file_sha256']
    if len(names) != len(set(names)) or set(names) != set(hashes):
        raise ValueError('selected files and hash keys must match exactly, without duplicates')
    if selection.get('unmatched_patterns') or selection.get('rejected_candidates'):
        raise ValueError('selection contains unresolved unmatched/rejected candidates')
    plan = {}
    for name in names:
        data, mode = read(source_root, name)
        if sha(data) != hashes[name]:
            raise ValueError(f'selected source hash mismatch: {name}')
        plan[name] = (data, mode, f'selected:{name}')

    relocation_bytes, _ = read(overlay_root, 'relocation-manifest.json')
    relocation = json.loads(relocation_bytes)
    overlays = {}
    for mapping in relocation['packaging']:
        target = mapping['destination']
        # Validate destinations with the same relative-path rule, without reading them.
        relative = PurePosixPath(target)
        if (relative.is_absolute() or not relative.parts or relative.as_posix() != target
                or '..' in relative.parts or target in overlays):
            raise ValueError(f'invalid or duplicate overlay destination: {target!r}')
        data, mode = read(overlay_root, mapping['source'])
        overlays[target] = (data, mode, f"overlay:{mapping['source']}")
    if overlays.get('relocation-manifest.json', (None,))[0] != relocation_bytes:
        raise ValueError('reviewed relocation manifest must be preserved at the package root')
    for change in relocation['changes']:
        source = change['source']
        if source not in plan or sha(plan[source][0]) != change['original_sha256']:
            raise ValueError(f'relocation original differs from selected source: {source}')
        for target, expected in [(change['executed_original'], change['original_sha256']),
                                 (change['overlay_target'], change['relocated_sha256'])]:
            if target not in overlays or sha(overlays[target][0]) != expected:
                raise ValueError(f'reviewed original/overlay hash mismatch: {target}')
    for source, expected in relocation.get('unchanged_sources', {}).items():
        if source not in plan or sha(plan[source][0]) != expected:
            raise ValueError(f'relocation unchanged-source hash mismatch: {source}')
    allowed_replacements = {change['overlay_target'] for change in relocation['changes']}
    for target in overlays:
        if target in plan and target not in allowed_replacements:
            raise ValueError(f'overlay unexpectedly replaces a selected file: {target}')
    plan.update(overlays)
    patch, mode = read(overlay_root, 'relocation.patch')
    if 'relocation.patch' in plan or MANIFEST in plan:
        raise ValueError('reserved package manifest/patch destination already selected')
    plan['relocation.patch'] = (patch, mode, 'overlay:relocation.patch')

    # All hashes and paths are checked before the destination is created. Write
    # these verified bytes so a changing source cannot alter the copied content.
    destination.mkdir(parents=True, exist_ok=False)
    files = {}
    for name, (data, mode, origin) in sorted(plan.items()):
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        target.chmod(mode)
        actual = target.read_bytes()
        if sha(actual) != sha(data):
            raise OSError(f'destination verification failed: {name}')
        files[name] = {'sha256': sha(actual), 'bytes': len(actual), 'source': origin}
    manifest = {'format': 1, 'selection_sha256': sha(selection_bytes),
                'selected_source_sha256': hashes,
                'relocation_manifest_sha256': sha(relocation_bytes),
                'relocation_patch_sha256': sha(patch),
                'file_count': len(files), 'bytes': sum(item['bytes'] for item in files.values()),
                'files': files, 'self_excluded': MANIFEST,
                'note': 'Hashes describe actual packaged bytes, including relocation overlays. '
                        'Packaging performs no builds, benchmark reruns, report/index edits or Git staging.'}
    (destination / MANIFEST).write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
    return manifest


def selfcheck():
    with tempfile.TemporaryDirectory(prefix='artifact-package-selfcheck-') as directory:
        root = Path(directory)
        source, overlay = root / 'source', root / 'overlay'
        source.mkdir()
        (source / 'runner.py').write_bytes(b'original\n')
        (source / 'log.txt').write_bytes(b'evidence\n')
        for name, data in {'originals/runner.py': b'original\n', 'overlays/runner.py': b'relocated\n',
                           'overlays/README.md': b'reproduction note\n', 'relocation.patch': b'patch\n'}.items():
            path = overlay / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        relocation = {'changes': [{'source': 'runner.py', 'executed_original': 'executed-sources/runner.py',
                                   'overlay_target': 'runner.py', 'original_sha256': sha(b'original\n'),
                                   'relocated_sha256': sha(b'relocated\n')}],
                      'unchanged_sources': {'log.txt': sha(b'evidence\n')},
                      'packaging': [{'source': a, 'destination': b} for a, b in [
                          ('originals/runner.py', 'executed-sources/runner.py'),
                          ('overlays/runner.py', 'runner.py'), ('overlays/README.md', 'README.md'),
                          ('relocation-manifest.json', 'relocation-manifest.json')]]}
        (overlay / 'relocation-manifest.json').write_text(json.dumps(relocation))
        selection = {'files': ['runner.py', 'log.txt'],
                     'file_sha256': {'runner.py': sha(b'original\n'), 'log.txt': sha(b'evidence\n')}}
        path, dest = root / 'selection.json', root / 'package'
        def rejected():
            try:
                package(path, source, overlay, dest)
            except ValueError:
                return
            raise AssertionError('invalid packaging request accepted')
        path.write_text(json.dumps(selection))
        rejected()
        assert not dest.exists()
        selection['finalized'] = True
        selection['file_sha256']['runner.py'] = 'wrong'
        path.write_text(json.dumps(selection))
        rejected()
        assert not dest.exists()
        selection['file_sha256']['runner.py'] = sha(b'original\n')
        path.write_text(json.dumps(selection))
        (overlay / 'overlays/runner.py').write_bytes(b'changed after review\n')
        rejected()
        assert not dest.exists()
        (overlay / 'overlays/runner.py').write_bytes(b'relocated\n')
        manifest = package(path, source, overlay, dest)
        assert (dest / 'runner.py').read_bytes() == b'relocated\n'
        assert (dest / 'executed-sources/runner.py').read_bytes() == b'original\n'
        assert (dest / 'relocation.patch').read_bytes() == b'patch\n'
        assert MANIFEST not in manifest['files']
        assert set(manifest['files']) == {str(p.relative_to(dest)) for p in dest.rglob('*') if p.is_file()} - {MANIFEST}
        for name, item in manifest['files'].items():
            assert sha((dest / name).read_bytes()) == item['sha256']
        rejected()
        assert (dest / 'runner.py').read_bytes() == b'relocated\n'
        try:
            read(source, '../selection.json')
            raise AssertionError('parent path accepted')
        except ValueError:
            pass
    print('Packaging selfcheck passed in a temporary directory; no repository files or real artifacts written.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selfcheck', action='store_true')
    for name in ('selection', 'source-root', 'overlay-root', 'destination'):
        parser.add_argument('--' + name, type=Path)
    args = parser.parse_args()
    if args.selfcheck:
        selfcheck()
        return
    if any(getattr(args, name) is None for name in ('selection', 'source_root', 'overlay_root', 'destination')):
        parser.error('--selection, --source-root, --overlay-root and --destination are required')
    manifest = package(args.selection, args.source_root, args.overlay_root, args.destination)
    print(f"Packaged {manifest['file_count']} files / {manifest['bytes']} bytes; {args.destination / MANIFEST}")


if __name__ == '__main__':
    main()
