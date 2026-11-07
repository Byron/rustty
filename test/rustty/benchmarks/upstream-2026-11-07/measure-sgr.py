"""Relocated SGR runner; original executed source is in executed-sources/."""
import argparse,subprocess,json,pathlib,hashlib,time
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--scratch',type=pathlib.Path,required=True,
                    help='Fresh run directory containing the five named frozen executables; results are written here.')
parser.add_argument('--input-root',type=pathlib.Path,required=True,
                    help='Directory containing plain.vt and styled-1.vt through styled-4.vt.')
args=parser.parse_args()
s=args.scratch.resolve();input_root=args.input_root.resolve()
cases=['plain','styled-1','styled-2','styled-3','styled-4'];rows=[]
configs=s/'stream-configs';configs.mkdir(parents=True,exist_ok=True)
for name in cases:
 config={'name':name,'data':str(input_root/f'{name}.vt'),'cols':120,'rows':80,
         'scrollback_bytes':10000,'grapheme_width':'legacy'}
 (configs/f'{name}.json').write_text(json.dumps(config,indent=2)+'\n')
meta={'started':time.time(),'compiler':subprocess.check_output(['rustc','+1.95.0','-vV'],text=True),'zig_version':subprocess.check_output(['zig','version'],text=True),'profile':'release thinLTO/codegen-units1; cargo --locked --offline','before':'373586cfd','binary_sha256':{n:hashlib.sha256((s/n).read_bytes()).hexdigest() for n in ['sgr-before-stream','sgr-after-stream','sgr-native-primitives','sgr-before-controlled','sgr-after-controlled']}}
for round in range(3):
 for name in cases:
  cfg=s/'stream-configs'/f'{name}.json';iters=100 if name=='plain' else 30
  for tag in (['before','after','native'] if round%2==0 else ['native','after','before']):
   argv=[str(s/'sgr-native-primitives'),'stream_file',str(cfg),str(iters)] if tag=='native' else [str(s/f'sgr-{tag}-stream'),str(cfg),str(iters)]
   r=subprocess.run(argv,capture_output=True,text=True);assert r.returncode==0,(tag,name,r.stderr)
   row={'round':round+1,'version':tag,'case':name,'argv':argv,'status':r.returncode,'result':json.loads(r.stdout),'stderr':r.stderr};rows.append(row)
   (s/'sgr-shared-timings.json').write_text(json.dumps(rows,indent=2)+'\n')
  print(f'shared round{round+1} {name} complete',flush=True)
controlled=[]
for case in ['plain','sgr-1','sgr-2','sgr-3','sgr-4']:
 iters=20000 if case=='plain' else 2000
 for tag in ['before','after']:
  argv=[str(s/f'sgr-{tag}-controlled'),case,str(iters),'80'];r=subprocess.run(argv,capture_output=True,text=True);assert r.returncode==0,(tag,case,r.stderr)
  controlled.append({'case':case,'version':tag,'argv':argv,'status':r.returncode,'results':[json.loads(l) for l in r.stdout.splitlines()],'stderr':r.stderr})
 (s/'sgr-controlled-timings.json').write_text(json.dumps(controlled,indent=2)+'\n');print(f'controlled {case} complete',flush=True)
meta['finished']=time.time();(s/'sgr-measurement-run.json').write_text(json.dumps(meta,indent=2)+'\n')
