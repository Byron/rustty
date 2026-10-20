#!/usr/bin/env python3
"""Observe C OSC 133 markers and native-equivalent output extraction."""
import json,os,re,subprocess,sys,time
from pathlib import Path
reference, fixture, output=sys.argv[1:]
cases=json.loads(Path(fixture).read_text())
results=[]
for case_index,case in enumerate(cases):
    base=[reference,'-L','txshell-%d-%d'%(os.getpid(),case_index),'-f','/dev/null']
    def run(*args,input=None):
        return subprocess.run(base+list(args),input=input,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True,timeout=10).stdout
    def ready(title):
        deadline=time.monotonic()+5
        while run('display-message','-p','#{pane_title}').strip()!=title:
            if time.monotonic()>deadline:raise RuntimeError('pane marker timed out')
            time.sleep(.01)
    observations=[]
    try:
        run('new-session','-d','-s','fixture','-x',str(case['cols']),'-y',str(case['rows']))
        run('set-option','-g','history-limit',str(case.get('history',2000)))
        run('new-window','-n','output',"stty raw -echo; printf '\033]2;READY\007'; exec cat")
        ready(b'READY')
        for index,step in enumerate(case['steps']):
            if 'feed' in step:
                title=('shell-%d'%index).encode()
                run('load-buffer','-b','input','-',input=bytes(step['feed'])+b'\x1b]2;'+title+b'\x07')
                run('paste-buffer','-b','input','-drS')
                ready(title)
            if 'resize' in step:run('resize-window','-x',str(step['resize']))
            capture=run('capture-pane','-p','-S','-')
            raw=run('capture-pane','-pR')
            markers=[]
            for line in raw.decode().splitlines():
                match=re.match(r'\tL (\d+) .* flags=(.*?)\[.*? osc133=(\d+),(\d+),(\d+),(\d+),(\d+)$',line)
                if match:
                    row,flags,prompt,command,start,end,status=match.groups()
                    flags=flags.split(',')
                    markers.append({'row':int(row),'prompt':[int(prompt),int('START_PROMPT' in flags)+2*int('SECOND_PROMPT' in flags)] if any(x in flags for x in ['START_PROMPT','SECOND_PROMPT']) else None,'command':int(command) if 'START_COMMAND' in flags else None,'start':int(start) if 'START_OUTPUT' in flags else None,'end':[int(end),int(status)] if 'END_OUTPUT' in flags else None})
            cursor=run('display-message','-p','#{cursor_x},#{cursor_y}').strip().decode()
            run('copy-mode')
            if 'search' in case:run('send-keys','-X','search-backward',case['search'])
            run('set-buffer','sentinel')
            run('send-keys','-X','copy-output')
            extracted=run('show-buffer')
            run('send-keys','-X','select-output')
            run('set-buffer','sentinel')
            run('send-keys','-X','copy-selection')
            selected=run('show-buffer')
            run('send-keys','-X','cancel')
            observations.append({'markers':markers,'capture':list(capture),'raw':raw.decode(),'cursor':list(map(int,cursor.split(','))),'output':None if extracted==b'sentinel' else list(extracted),'selection':None if selected==b'sentinel' else list(selected)})
        results.append(observations)
    finally:
        subprocess.run(base+['kill-server'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
Path(output).write_text(json.dumps(results))
