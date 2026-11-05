from pathlib import Path
import json, statistics, collections
out=Path(__file__).parent
rows=collections.defaultdict(dict)
for execution in map(json.loads,(out/'execution.jsonl').read_text().splitlines()):
    name=execution['name']; data=list(map(json.loads,(out/(name+'.jsonl')).read_text().splitlines())); setup,op=data
    group=name.split('-')[0]; key=(group,setup['rows'],setup['kind'],op['case'])
    rows[key][(execution['pair'],execution['variant'])]=(setup,op)
summary=[]
for (group,count,kind,case),data in rows.items():
    variants=['baseline','metadata'] if group=='metadata' else ['metadata','search']
    if len(data)!=6: continue
    item={'group':group,'rows':count,'kind':kind,'case':case,'variants':{}}
    for variant in variants:
        values=[data[(p,variant)][1] for p in range(3)]
        item['variants'][variant]={key:statistics.median(v[key] for v in values) for key in ['cold_ns','warm_ns','before_owned','after_owned','recompressed_owned','recompress_ns','recompress_step_max_ns']}
        item['variants'][variant]['raw_cold_ns']=[v['cold_ns'] for v in values]
        item['variants'][variant]['raw_warm_ns']=[v['warm_ns'] for v in values]
    for pair in range(3):
        a,b=(data[(pair,v)][1] for v in variants)
        assert a['result']==b['result'] and a['again']==b['again'],(group,count,kind,case,pair)
        assert a['before_owned']==b['before_owned'] and a['recompressed_owned']==a['before_owned'] and b['recompressed_owned']==b['before_owned']
        assert a['result']==a['again']==b['result']==b['again']==1
        assert a['after_owned']==b['after_owned']
    item['paired_cold_ratio']=[data[(p,variants[1])][1]['cold_ns']/data[(p,variants[0])][1]['cold_ns'] for p in range(3)]
    item['paired_warm_ratio']=[data[(p,variants[1])][1]['warm_ns']/data[(p,variants[0])][1]['warm_ns'] for p in range(3)]
    summary.append(item)
(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
for s in summary:
    a,b=s['variants'].values()
    print(s['group'],s['rows'],s['kind'],s['case'],f"cold {a['cold_ns']/1e6:.4f}->{b['cold_ns']/1e6:.4f}ms warm {a['warm_ns']/1e6:.4f}->{b['warm_ns']/1e6:.4f}ms",f"owned_delta {a['after_owned']-a['before_owned']}->{b['after_owned']-b['before_owned']}")
