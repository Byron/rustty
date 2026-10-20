#!/usr/bin/env python3
"""Verify an actual hidden Windows native frame with signed, clipped floating geometry."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import platform
import struct
import sys
import tempfile
import uuid
from windows_native_views import Fixture, sha256, wait_json


def exercise(fixture, report):
    directory = fixture.directory / 'frame'
    directory.mkdir()
    env = fixture.env | {'RUSTTY_TMUXX_SMOKE_DIR': str(directory),
                         'RUSTTY_TMUXX_SMOKE_FLOATING': '1'}
    env.pop('RUSTTY_TMUXX_SMOKE_VISIBLE', None)
    gui = fixture.start([str(fixture.gui), '--tmuxx=embedded',
                         '--tmuxx-instance=hidden-frame-' + uuid.uuid4().hex,
                         '--renderer=software', '--config-default-files=false', '--stream-deck=false'],
                        'hidden-frame', env)
    state = wait_json(directory / 'result.json', gui)
    fixture.remember(state)
    assert state['panes'] == 2 and state['windows'] == 1 and state['floating_panes'] == 1, state
    assert state['hidden'] and not state['focused'] and not state['errors'], state
    assert state['view_graph_consistent'] and state['instance'], state
    assert len(state['draw_order']) == len(set(state['draw_order'])) == 2, state
    assert 'software' in state['renderer'].lower(), state['renderer']
    # The native workspace retains the signed origin; clipping is renderer-owned.
    floating = state['workspace']['windows'][0]['tabs'][0]['overlays']
    assert len(floating) == 1, floating
    item = floating[0]
    assert item['x'] == -1 and item['y'] == 5 and item['cols'] == 40 and item['rows'] == 8, item
    assert state['draw_order'][-1] == item['pane'], 'floating pane must render last'
    report['floating_geometry'] = item
    png_path = directory / 'window.png'
    png = png_path.read_bytes()
    assert png.startswith(b'\x89PNG\r\n\x1a\n') and len(png) > 1000
    width, height = struct.unpack('>II', png[16:24])
    assert width > 100 and height > 100
    assert gui.wait(timeout=20) == 0
    fixture.assert_dead(state['pane_processes'].values())
    report.update(renderer=state['renderer'], state=state, frame_size=[width, height],
                  frame_sha256=sha256(png_path), frame_path=str(png_path))
    fixture.checks.append('desktop::hidden_native_frame')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['gui', 'candidate', 'report']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'win32':
        parser.error('requires native Windows')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix='windows-native-frame-', dir=args.report.parent))
    fixture = Fixture(args.gui.resolve(strict=True), args.candidate.resolve(strict=True), directory)
    report = {'schema': 1, 'kind': 'windows-native-frame', 'artifact_directory': str(directory),
              'os': platform.platform(), 'architecture': platform.machine(),
              'gui_sha256': sha256(fixture.gui), 'candidate_sha256': sha256(fixture.candidate),
              'tests': [], 'failures': [], 'cleanup_remaining': []}
    try:
        exercise(fixture, report)
    except (Exception, KeyboardInterrupt) as error:
        report['failures'].append(f'{type(error).__name__}: {error}')
    finally:
        report['tests'] = fixture.checks
        report['cleanup_remaining'] = fixture.cleanup()
        report['passed'] = not report['failures'] and not report['cleanup_remaining']
        args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(report))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
