#!/usr/bin/env python3
"""Benchmark the full Rust CLI against its archived JS predecessor; no live models."""
import argparse, hashlib, json, os, platform, random, statistics, subprocess, tempfile, time
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
p = argparse.ArgumentParser()
p.add_argument('--binary', default=str(ROOT / 'target/release/lennon'))
p.add_argument('--fixture', default=str(ROOT / 'target/debug/lennon-provider-fixture'))
p.add_argument('--output', default=str(ROOT / 'benchmarks/results/native-port-local.json'))
a = p.parse_args()
RUST = str(Path(a.binary).resolve())
JS = ['node', str(ROOT / 'benchmarks/reference-js/bin/lennon.js')]
rng = random.Random(0x6c656e6e)
def run(cmd, env=None):
    start = time.perf_counter_ns()
    r = subprocess.run(cmd, cwd=ROOT, input=b'', capture_output=True, env=env, timeout=30)
    elapsed = (time.perf_counter_ns() - start) / 1e6
    if r.returncode: raise RuntimeError(f'{cmd}: {r.returncode}: {r.stderr.decode()} {r.stdout.decode()}')
    return elapsed, r

def summary(values):
    v=sorted(values)
    def q(p):
        i=(len(v)-1)*p; low=int(i); high=min(low+1,len(v)-1)
        return v[low]+(v[high]-v[low])*(i-low)
    return {'n':len(v),'median':statistics.median(v),'p95':q(.95),'min':min(v),'max':max(v)}

result={'environment':{'machine':platform.machine(),'os':platform.platform(),'node':subprocess.check_output(['node','--version'],text=True).strip(),'rustCli':subprocess.check_output([RUST,'--version'],text=True).strip()},'methodology':{'warmups':5,'sampling':'balanced randomized blocks, warm filesystem caches, fresh process per sample','durability':'Same native full-sync policy after every journal event; no batching','rustBinaryBytes':Path(RUST).stat().st_size,'rustBinarySha256':hashlib.sha256(Path(RUST).read_bytes()).hexdigest()},'cases':{}}
startup={'rust':[],'node_js':[]}
for i in range(-5,100):
    names=list(startup);rng.shuffle(names)
    for name in names:
        elapsed,_=run(([RUST] if name=='rust' else JS)+['--help'])
        if i>=0:startup[name].append(elapsed)
result['cases']['startup']={k:{'wallMs':summary(v),'samples':v}for k,v in startup.items()}
print('startup',json.dumps({k:v['wallMs']for k,v in result['cases']['startup'].items()}),flush=True)

with tempfile.TemporaryDirectory(prefix='lennon-native-port-') as temporary:
    root=Path(temporary);cfg=root/'config.json'; capture=root/'capture.jsonl'
    fixture=str(Path(a.fixture).resolve())
    cfg.write_text(json.dumps({'order':['claude','codex'],'providers':{n:{'command':fixture}for n in ('claude','codex')}}))
    def env(state,scenario):
        return dict(os.environ,LENNON_CONFIG=str(cfg),LENNON_STATE_DIR=str(state),LENNON_TEST_CAPTURE=str(capture),LENNON_TEST_SCENARIO=scenario)
    # Prove an actual JS session resumes in Rust, then resumes back in JS.
    state=root/'interop'
    _,first=run(JS+['--json','--provider','codex','-p','Original compatibility constraint'],env(state,'success'))
    sid=json.loads(first.stdout.splitlines()[0])['session']
    _,second=run([RUST,'--json','--resume',sid,'--provider','codex','-p','Continue in Rust'],env(state,'success'))
    _,third=run(JS+['--json','--resume',sid,'--provider','codex','-p','Continue back in JavaScript'],env(state,'success'))
    calls=[json.loads(l)for l in capture.read_text().splitlines()]
    assert 'resume' in calls[1]['args'] and 'resume' in calls[2]['args']
    assert 'codex-default-native' in calls[1]['args'] and 'codex-default-native' in calls[2]['args']
    result['sessionCompatibility']={'jsToRust':'passed','rustToJs':'passed','sameNativeSession':True}
    print('session compatibility: JS -> Rust -> JS passed',flush=True)

    samples={'rust':[],'node_js':[]}
    for i in range(-5,25):
        names=list(samples);rng.shuffle(names)
        for name in names:
            state=root/f'{name}-{i}'; capture.unlink(missing_ok=True)
            command=([RUST]if name=='rust'else JS)+['--json','-p','benchmark task: continue; never deploy']
            elapsed,out=run(command,env(state,'failover'))
            events=[json.loads(l)for l in out.stdout.splitlines()]
            last=next(e for e in events if e['type']=='result')
            assert last['status']=='success' and last['provider']=='codex'
            calls=[json.loads(l)for l in capture.read_text().splitlines()]
            assert len(calls)==2 and 'app.rs already updated' in calls[1]['prompt'] and 'never deploy' in calls[1]['prompt']
            if i>=0:samples[name].append(elapsed)
    result['cases']['full_failover']={k:{'wallMs':summary(v),'samples':v}for k,v in samples.items()}
    print('full failover',json.dumps({k:v['wallMs']for k,v in result['cases']['full_failover'].items()}),flush=True)

result['memory']={}
if platform.system()=='Darwin':
    for name,command in [('rust',[RUST]),('node_js',JS)]:
        rss=[]
        for _ in range(5):
            _,r=run(['/usr/bin/time','-l']+command+['--help'])
            line=next(l for l in r.stderr.decode().splitlines()if 'maximum resident set size'in l)
            rss.append(int(line.split()[0])/1024/1024)
        result['memory'][name]={'peakRssMiB':summary(rss)}
Path(a.output).parent.mkdir(parents=True,exist_ok=True)
Path(a.output).write_text(json.dumps(result,indent=2)+'\n')
print(a.output)
