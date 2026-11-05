import json, os, subprocess, sys
from pathlib import Path
binary, variant, mode = sys.argv[1:]
root=Path('/tmp/rustty-capture-results-2026-11-05')
root.mkdir(exist_ok=True)
names=[
'capture_sgr_color_origins_spaces_escaping_and_ranges_match_c',
'capture_alternate_history_and_missing_screen_match_c',
'capture_hyperlink_lists_numbers_and_flag_precedence_match_c',
'retained_tabs_capture_overwrite_erase_shift_and_snapshot_match_c',
'retained_tabs_reflow_at_normal_widths_matches_c',
]
env=dict(os.environ,TMUXX_C_TMUX='/usr/bin/tmux')
if mode=='differential':env['RUSTTY_CAPTURE_DIFFERENTIAL']='1'
for name in names:
    result=subprocess.run([binary,name,'--ignored','--nocapture','--test-threads=1'],env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
    (root/f'{variant}-{mode}-{name}.log').write_text(result.stdout)
    cases=[json.loads(line.split('CAPTURECASE ',1)[1]) for line in result.stdout.splitlines() if 'CAPTURECASE ' in line]
    (root/f'{variant}-{mode}-{name}.json').write_text(json.dumps(cases,indent=2)+'\n')
    print(name, 'exit',result.returncode,'cases',len(cases),'C36 comparisons',result.stdout.count('C36_COMPARE'),'excluded',result.stdout.count('C36_SKIP'),flush=True)
    if result.returncode: sys.exit(result.returncode)
