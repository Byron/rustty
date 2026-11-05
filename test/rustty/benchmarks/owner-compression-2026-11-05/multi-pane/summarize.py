from pathlib import Path
import json
import re
import statistics

out = Path(__file__).parent
runs = [json.loads(s) for s in (out / 'execution.jsonl').read_text().splitlines()]
data = {}
for run in runs:
    assert run['returncode'] == 0
    d = json.loads((out / (run['name'] + '.json')).read_text())
    assert len(d['stages']) == len(d['captures_after_memory']) == 4
    assert all(c['old_and_final_text'] for c in d['captures_after_memory'])
    for n, stage in enumerate(d['stages']):
        assert stage['pane_count'] == n + 1
        assert stage['sum_owned_bytes'] == sum(p['owned_bytes'] for p in stage['panes'])
        assert all(p['history_rows'] == d['rows'] - 31 for p in stage['panes'])
    data[(run['pair'], run['variant'])] = d
for pair in range(3):
    a, b = (data[(pair, v)] for v in ['before', 'after'])
    assert a['input_bytes_per_pane'] == b['input_bytes_per_pane']
    assert a['captures_after_memory'] == b['captures_after_memory']
summary = {}
for variant in ['before', 'after']:
    ds = [data[(pair, variant)] for pair in range(3)]
    stages = []
    for n in range(4):
        stages.append({k: statistics.median(d['stages'][n][k] for d in ds) for k in [
            'creation_ns', 'ready_ns', 'feed_to_owner_done_ns', 'sum_owned_bytes', 'process_rss_bytes']})
    cpu = []
    for run in runs:
        if run['variant'] != variant:
            continue
        raw = (out / (run['name'] + '.time')).read_text()
        cpu.append({k: float(re.search(re.escape(k) + r':\s*([0-9.]+)', raw).group(1)) for k in [
            'User time (seconds)', 'System time (seconds)', 'Maximum resident set size (kbytes)']})
    summary[variant] = {'stages': stages, 'whole_job': {k: statistics.median(d[k] for d in cpu) for k in cpu[0]}}
summary['paired_ratios'] = {k: [[data[(p,'after')]['stages'][n][k] / data[(p,'before')]['stages'][n][k] for n in range(4)] for p in range(3)] for k in ['creation_ns','feed_to_owner_done_ns','process_rss_bytes']}
(out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
