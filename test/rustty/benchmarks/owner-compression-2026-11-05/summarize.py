from pathlib import Path
import json,statistics,math,collections
out=Path(__file__).parent
executions=[json.loads(s) for s in (out/'execution.jsonl').read_text().splitlines()]
groups=collections.defaultdict(dict)
def quantile(values,q):
    values=sorted(values)
    return values[max(0,math.ceil(q*len(values))-1)]
for execution in executions:
    assert execution['returncode']==0
    data=json.loads((out/(execution['name']+'.json')).read_text())
    assert data['initial_memory']['history_rows']==data['after_capture']['history_rows']==data['after_snapshot']['history_rows']
    assert data['initial_memory']['owned_bytes']==data['after_capture']['owned_bytes']==data['after_snapshot']['owned_bytes']
    groups[(execution['mode'],execution['rows'],execution['kind'])][(execution['pair'],execution['variant'])]=data
summary=[]
for (mode,rows,kind),runs in groups.items():
    entry={'mode':mode,'rows':rows,'kind':kind,'variants':{},'paired_ratios':{}}
    for variant in ['before','after']:
        ds=[runs[(pair,variant)] for pair in range(3)]
        v={key:statistics.median(d[key] for d in ds) for key in ['input_bytes','feed_to_owner_done_ns','cold_capture_ns','cold_native_attach_ns','capture_bytes']}
        for key in ['initial_memory','after_capture','after_snapshot']:
            v[key]={field:statistics.median(d[key][field] for d in ds) for field in ['owned_bytes','history_rows','process_rss_bytes','elapsed_ns']}
        for group in ['initial_idle','post_feed_samples','capture_activity']:
            v[group]={}
            for metric in ['command_ns','echo_observed_ns','child_dsr_ns']:
                samples=[[item['sample'][metric] if group=='capture_activity' else item[metric] for item in d[group]] for d in ds]
                v[group][metric]={'per_run_p50':[quantile(s,.5) for s in samples], 'per_run_p99':[quantile(s,.99) for s in samples], 'per_run_max':[max(s) for s in samples]}
                v[group][metric]['p50_median']=statistics.median(v[group][metric]['per_run_p50'])
                v[group][metric]['p99_median']=statistics.median(v[group][metric]['per_run_p99'])
                v[group][metric]['max_all']=max(v[group][metric]['per_run_max'])
        peaks=[]
        for ex in executions:
            if (ex['mode'],ex['rows'],ex['kind'],ex['variant'])==(mode,rows,kind,variant):
                for line in (out/(ex['name']+'.time')).read_text().splitlines():
                    if 'Maximum resident set size (kbytes):' in line:peaks.append(int(line.rsplit(':',1)[1])*1024)
        v['job_maxrss_bytes']=peaks
        entry['variants'][variant]=v
    for pair in range(3):
        a,b=(runs[(pair,v)] for v in ['before','after'])
        assert a['initial_memory']['history_rows']==b['initial_memory']['history_rows']
        assert a['input_bytes']==b['input_bytes'] and a['capture_bytes']==b['capture_bytes']
    for key in ['feed_to_owner_done_ns','cold_capture_ns','cold_native_attach_ns']:
        entry['paired_ratios'][key]=[runs[(p,'after')][key]/runs[(p,'before')][key] for p in range(3)]
    summary.append(entry)
(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
for s in summary:
    a,b=[s['variants'][v] for v in ['before','after']]
    print(s['mode'],s['rows'],s['kind'],'owned',a['initial_memory']['owned_bytes'],'->',b['initial_memory']['owned_bytes'],'RSS',a['initial_memory']['process_rss_bytes'],'->',b['initial_memory']['process_rss_bytes'])
    print('  feed ms',round(a['feed_to_owner_done_ns']/1e6,2),'->',round(b['feed_to_owner_done_ns']/1e6,2),'capture ms',round(a['cold_capture_ns']/1e6,2),'->',round(b['cold_capture_ns']/1e6,2),'attach ms',round(a['cold_native_attach_ns']/1e6,2),'->',round(b['cold_native_attach_ns']/1e6,2))
    for metric in ['command_ns','child_dsr_ns','echo_observed_ns']:
        x=a['capture_activity'][metric];y=b['capture_activity'][metric]
        print(' ',metric,'capture p99/max ms',round(x['p99_median']/1e6,3),round(x['max_all']/1e6,3),'->',round(y['p99_median']/1e6,3),round(y['max_all']/1e6,3))
