from pathlib import Path
import json, subprocess, sys, time
out=Path(__file__).parent
mode=sys.argv[1]
rows=int(sys.argv[2]); kind=sys.argv[3]
for pair in range(3):
    variants=['before','after'] if pair%2==0 else ['after','before']
    for position,variant in enumerate(variants):
        name=f'{mode}-{rows}-{kind}-{pair}-{position}-{variant}'
        directory=out/name
        args=['/usr/bin/time','-v','-o',str(out/(name+'.time')),f'/tmp/rustty-owner-{variant}-bin',str(directory),mode,str(rows),kind]
        started=time.time_ns()
        completed=subprocess.run(args,capture_output=True,text=True)
        (out/(name+'.json')).write_text(completed.stdout)
        (out/(name+'.stderr')).write_text(completed.stderr)
        with (out/'execution.jsonl').open('a') as f:f.write(json.dumps({'name':name,'args':args,'pair':pair,'position':position,'variant':variant,'mode':mode,'rows':rows,'kind':kind,'start_unix_ns':started,'end_unix_ns':time.time_ns(),'returncode':completed.returncode})+'\n')
        if completed.returncode: raise RuntimeError(f'{name}: {completed.stderr}')
    print(mode,rows,kind,'pair',pair,'complete',flush=True)
