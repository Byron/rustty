#!/usr/bin/env python3
"""Capture C redraw pixels for the shared Kitty lifetime fixture."""
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(os.environ.get('TMUXX_C_SOURCE', Path(__file__).resolve().parents[2])) / 'regress'))
from image_test import image_test
from image_terminal import Terminal

reference, fixture, output = sys.argv[1:]
plan = json.loads(Path(fixture).read_text())
observations = []
with image_test(reference, sx=plan['cols'], sy=plan['rows']) as test:
    client = test.attach(cw=8, ch=16)
    for step in plan['steps']:
        if 'feed' in step:
            test.emit(bytes(step['feed']))
        elif 'limit' in step:
            test.run('set-option', '-s', 'image-size-limit', str(step['limit']))
        elif 'resize' in step:
            test.resize(client, *step['resize'])
        elif step.get('respawn'):
            test.run('respawn-pane', '-k', "stty raw -echo; printf '\033]2;RESPAWNED\007'; exec cat")
            test.wait(lambda: test.run('display-message', '-p', '#{pane_title}') == 'RESPAWNED')
        test.redraw()
        cols, rows = map(int, test.run('display-message', '-p', '#{pane_width},#{pane_height}').split(','))
        terminal = Terminal(sx=cols, sy=rows, cw=8, ch=16)
        terminal.feed(client.data)
        observations.append({'cols':cols, 'rows':rows,
            'cursor': list(map(int,test.run('display-message','-p','#{cursor_x},#{cursor_y}').split(','))),
            'pixels': [[x,y,*color] for (x,y),color in sorted(terminal.pixels.items())]})
Path(output).write_text(json.dumps(observations))
