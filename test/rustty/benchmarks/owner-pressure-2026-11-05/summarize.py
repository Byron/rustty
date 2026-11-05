from pathlib import Path
import json, math, statistics
root = Path(__file__).parent

def stats(values):
    values = sorted(values)
    if not values:
        return {'count': 0}
    return {'count': len(values), 'median_ms': statistics.median(values) / 1e6,
            'p99_ms': values[math.ceil(len(values) * .99) - 1] / 1e6,
            'max_ms': values[-1] / 1e6}

result = []
for scenario in ['native-two', 'embedded-three', 'native-three']:
    for variant in ['before', 'after']:
        name = f'{variant}-{scenario}-100k'
        data = json.loads((root / f'{name}.json').read_text())
        samples = [sample for burst in data['pressure'] for sample in burst['samples']]
        row = {'case': name, 'bursts': data['bursts'],
               'recovery': data['recovery'], 'reconnect_equal': data['reconnect_equal'],
               'retained_rows': data['retained_rows'],
               'recovery_history_equal': data['recovery_history_equal'],
               'reconnect_history_equal': data['reconnect_history_equal'],
               'blocked_interval_s': sum(burst['elapsed_ns'] for burst in data['pressure']) / 1e9,
               'bulk_command': stats([sample['command_ns'] for sample in samples]),
               'bulk_echo': stats([sample['echo_view_ns'] for sample in samples]),
               'catchup_command': stats(data['recovery_command_ns'])}
        for line in (root / f'{name}.time').read_text().splitlines():
            for prefix, key in [('User time (seconds):', 'user_s'),
                                ('System time (seconds):', 'system_s'),
                                ('Maximum resident set size (kbytes):', 'rss_kib')]:
                if line.strip().startswith(prefix):
                    row[key] = float(line.split(':', 1)[1].strip())
        result.append(row)
(root / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
for row in result:
    print(row['case'], 'recover=', row['recovery'], 'rows=', row['retained_rows'],
          'history=', row['recovery_history_equal'], row['reconnect_history_equal'],
          'command=', row['bulk_command'], 'echo=', row['bulk_echo'],
          'catchup=', row['catchup_command'], 'CPU=', row['user_s'], row['system_s'],
          'RSS_MiB=', row['rss_kib']/1024)
