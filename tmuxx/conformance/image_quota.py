#!/usr/bin/env python3
"""Replay ordered image protocol steps in separate panes of an isolated C server."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

reference, fixture, output = sys.argv[1:]
plan = json.loads(Path(fixture).read_text())
with tempfile.TemporaryDirectory(prefix='txquota-', dir='/tmp') as temporary:
    directory = Path(temporary)
    command = [reference, '-f', '/dev/null', '-S', str(directory / 's')]
    def run(*args):
        return subprocess.check_output(command + list(args), stderr=subprocess.PIPE, env={k:v for k,v in os.environ.items() if k != 'TMUX'})
    (directory / 'reader.py').write_text('''import os,select,sys,time,tty
from pathlib import Path
tty.setraw(0)
directory=Path(sys.argv[1]);pane=sys.argv[2]
os.write(1,b'\\x1b]2;READY\\x07')
for index in range(1000):
    source=directory/f'in-{pane}-{index}'
    deadline=time.monotonic()+30
    while not source.exists():
        if time.monotonic()>deadline: raise SystemExit('missing fixture step')
        time.sleep(0.002)
    data=source.read_bytes()+b'\\x1b[c'
    while data: data=data[os.write(1,data):]
    reply=b''
    while time.monotonic()<deadline:
        if select.select([0],[],[],0.05)[0]:
            reply+=os.read(0,65536)
            if b'\\x1b[?' in reply and reply.endswith(b'c'):
                out=directory/f'out-{pane}-{index}'
                out.with_suffix('.tmp').write_bytes(reply.split(b'\\x1b[?')[0])
                out.with_suffix('.tmp').rename(out)
                break
    else: raise SystemExit('missing DA barrier')
''')
    try:
        run('new-session', '-d', '-x', '40', '-y', '16', 'sleep 60')
        assert run('display-message', '-p', '#{image_support}') == b'1\n'
        for name,value in [('image-total-count-limit',plan['count']),('image-total-size-limit',plan['bytes'])]:
            run('set-option','-s',name,str(value))
        for pane in range(2):
            child=f'/usr/bin/python3 -B {directory}/reader.py {directory} {pane}'
            run('new-window','-d',child)
        indices=[0,0];observations=[]
        for step in plan['steps']:
            pane=step['pane'];index=indices[pane];indices[pane]+=1
            source=directory/f'in-{pane}-{index}'
            source.with_suffix('.tmp').write_bytes(bytes(step['feed']))
            source.with_suffix('.tmp').rename(source)
            result=directory/f'out-{pane}-{index}'
            deadline=time.monotonic()+12
            while not result.exists():
                if time.monotonic()>deadline: raise RuntimeError('missing quota observation')
                time.sleep(0.003)
            observations.append(list(result.read_bytes()))
        Path(output).write_text(json.dumps(observations))
    finally:
        subprocess.run(command+['kill-server'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
