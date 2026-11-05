from pathlib import Path
import json, subprocess, sys, time
out=Path(__file__).parent
manifest=json.loads((out/'manifest.json').read_text())
group=sys.argv[1]; rows=int(sys.argv[2]); kinds=sys.argv[3:] or ['ascii','unicode']
variants=['baseline','metadata'] if group=='metadata' else ['metadata','search']
for kind in kinds:
    for case in manifest['operations'][group]:
        for pair in range(3):
            order=variants if pair%2==0 else list(reversed(variants))
            for position,variant in enumerate(order):
                name=f'{group}-{rows}-{kind}-{case}-{pair}-{position}-{variant}'
                args=[manifest['binaries'][variant]['path'],str(rows),kind,case]
                start=time.time_ns(); result=subprocess.run(args,capture_output=True,text=True,check=True)
                (out/(name+'.jsonl')).write_text(result.stdout)
                (out/(name+'.stderr')).write_text(result.stderr)
                with (out/'execution.jsonl').open('a') as f: f.write(json.dumps({'name':name,'args':args,'start_unix_ns':start,'end_unix_ns':time.time_ns(),'pair':pair,'position':position,'variant':variant})+'\n')
        print(group,rows,kind,case,'3 pairs complete',flush=True)
