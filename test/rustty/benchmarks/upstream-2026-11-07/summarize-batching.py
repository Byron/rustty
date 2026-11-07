from pathlib import Path
import json,statistics,sys
s=Path('/private/tmp/rustty-reintegrate.leadij');name=sys.argv[1]
rows=json.loads((s/(name+'-timings.json')).read_text());summary=[]
for case in dict.fromkeys(x['name'] for x in rows):
 entry={'name':case}
 for version in dict.fromkeys(x['version'] for x in rows if x['name']==case):
  selected=[x['result'] for x in rows if x['name']==case and x['version']==version]
  samples=[x['elapsed_ns']/x['iterations']/1e6 for x in selected]
  entry[version]={'ms':statistics.median(samples),'samples_ms':samples,**{k:selected[0][k] for k in ['history_rows','page_bytes','page_count','owned_bytes']}}
 entry['change_percent']=(entry['after']['ms']/entry['before']['ms']-1)*100
 if 'pre-blank' in entry:entry['combined_change_percent']=(entry['after']['ms']/entry['pre-blank']['ms']-1)*100
 summary.append(entry)
 print(case,round(entry['before']['ms'],3),'->',round(entry['after']['ms'],3),'ms',f"{entry['change_percent']:+.1f}%", f"combined {entry['combined_change_percent']:+.1f}%" if 'pre-blank' in entry else '')
(s/(name+'-measurement-summary.json')).write_text(json.dumps(summary,indent=2)+'\n')
