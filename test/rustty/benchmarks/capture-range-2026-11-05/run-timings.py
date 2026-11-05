from pathlib import Path
import json, statistics, subprocess, time
root=Path('/tmp/rustty-capture-results-2026-11-05/timing')
root.mkdir(exist_ok=True)
order=[]
for rows in [20000,100000]:
    for pair,variants in [(1,['before','after']),(2,['after','before']),(3,['before','after'])]:
        for variant in variants:
            name=f'{variant}-{rows}-{pair}'
            with (root/f'{name}.json').open('w') as output:
                result=subprocess.run(['/usr/bin/time','-v','-o',str(root/f'{name}.time'),f'/tmp/rustty-capture-{variant}-bin',str(rows),'5'],stdout=output,stderr=subprocess.PIPE,text=True)
            if result.returncode:
                (root/f'{name}.stderr').write_text(result.stderr)
                raise SystemExit(result.returncode)
            data=json.loads((root/f'{name}.json').read_text())
            order.append({'name':name,'rows':rows,'variant':variant,'pair':pair})
            (root/'order.json').write_text(json.dumps(order,indent=2)+'\n')
            medians={item['state']+'/'+item['case']:round(statistics.median(item['samples_ns'])/1e6,4) for item in data['cases']}
            print(name,json.dumps(medians),flush=True)
