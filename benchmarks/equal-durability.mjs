// Follow-up experiment: compare the same macOS F_FULLFSYNC policy explicitly.
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, mkdirSync, rmSync, cpSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { createHash } from 'node:crypto';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const bun = process.env.LENNON_BENCH_BUN;
if (!bun || process.platform !== 'darwin') throw new Error('Requires macOS and LENNON_BENCH_BUN');
const rust = process.env.LENNON_BENCH_RUST || join(root,'benchmarks/rust/target/release/lennon-perf-probe');
const work = mkdtempSync(join(tmpdir(),'lennon-fullsync-'));
const result = { generated:new Date().toISOString(), policy:'macOS F_FULLFSYNC after every event; benchmark-only Bun FFI shim, no batching', warmups:5, sampleOrder:'balanced randomized blocks', cases:{} };
let seed=0x6c656e6e;
function shuffle(a) { for(let i=a.length-1;i>0;i--) { seed=(Math.imul(seed,1664525)+1013904223)>>>0; const j=seed%(i+1); [a[i],a[j]]=[a[j],a[i]]; } return a; }
function q(v,p) { const a=[...v].sort((a,b)=>a-b); const i=(a.length-1)*p; return a[Math.floor(i)]+(a[Math.ceil(i)]-a[Math.floor(i)])*(i%1); }
function stats(v) { return {n:v.length,median:q(v,.5),p95:q(v,.95),min:Math.min(...v),max:Math.max(...v)}; }
function run(command,args,env={}) {
  const start=performance.now();
  const p=spawnSync(command,args,{cwd:root,encoding:'utf8',env:{...process.env,...env},maxBuffer:32*1024*1024});
  const elapsed=performance.now()-start;
  if(p.error || p.status!==0) throw new Error(p.error?.message || p.stderr || p.stdout);
  return {elapsed,stdout:p.stdout};
}
try {
  const rows=Array.from({length:100},(_,i)=>JSON.stringify({type:'stream_event',event:{type:'content_block_delta',delta:{type:'text_delta',text:`hello 🐕 ${i} ${'x'.repeat(80)}`}}}));
  const input=join(work,'events.jsonl'); writeFileSync(input,rows.join('\n')+'\n');
  const samples={}; let expectedDigest;
  for(const mode of ['journal','journal-open']) {
    const variants=[['node',process.execPath,['benchmarks/probe.mjs'],{}],['rust',rust,[],{}],['bun_fullsync',bun,['benchmarks/probe.mjs'],{LENNON_BENCH_FULLSYNC:'1'}]];
    for(const [name] of variants) samples[`${name}_${mode}`]={wall:[],work:[]};
    for(let i=-5;i<15;i++) for(const [name,command,prefix,env] of shuffle([...variants])) {
      const output=join(work,'journal.jsonl'); rmSync(output,{force:true});
      const p=run(command,[...prefix,mode,input,output],env); const data=JSON.parse(p.stdout);
      if(data.events!==100) throw new Error('Event count mismatch');
      const digest=createHash('sha256').update(readFileSync(output)).digest('hex');
      expectedDigest ||= digest;
      if(digest!==expectedDigest) throw new Error('Journal bytes mismatch');
      if(i>=0) { samples[`${name}_${mode}`].wall.push(p.elapsed); samples[`${name}_${mode}`].work.push(data.workMs); }
    }
  }
  result.cases.journal_100 = Object.fromEntries(Object.entries(samples).map(([name,v])=>[name,{wallMs:stats(v.wall),workMs:stats(v.work),samples:v.wall,workSamples:v.work}]));
  result.digest=expectedDigest;
  console.log('equal-durability journals',JSON.stringify(result.cases.journal_100));

  // Overlay only the fsync binding in a temporary copy; all other source is identical.
  // The shipping repository is never modified and no FFI shim ships in Lennon.
  const overlay=join(work,'overlay'); mkdirSync(overlay);
  cpSync(join(root,'src'),join(overlay,'src'),{recursive:true});
  cpSync(join(root,'bin'),join(overlay,'bin'),{recursive:true});
  cpSync(join(root,'package.json'),join(overlay,'package.json'));
  const store=join(overlay,'src/store.js');
  writeFileSync(store,`import {sync as fsyncSync} from ${JSON.stringify(pathToFileURL(join(root,'benchmarks/fullsync.mjs')).href)};\n`+readFileSync(store,'utf8').replace('fsyncSync, ',''));
  const primary=join(work,'provider-claude'),fallback=join(work,'provider-codex');
  symlinkSync(rust,primary); symlinkSync(rust,fallback);
  const config=join(work,'config.json');
  writeFileSync(config,JSON.stringify({order:['claude','codex'],providers:{claude:{command:primary},codex:{command:fallback}}}));
  const pipeline={node:{total:[],handoff:[]},bun_fullsync:{total:[],handoff:[]}};
  for(let i=-5;i<20;i++) for(const name of shuffle(['node','bun_fullsync'])) {
    const dir=mkdtempSync(join(work,'attempt-'));
    const executable=name==='node'?process.execPath:bun;
    const entry=name==='node'?join(root,'bin/lennon.js'):join(overlay,'bin/lennon.js');
    const p=run(executable,[entry,'--json','--cwd',root,'-p','benchmark task: finish existing work'],{LENNON_CONFIG:config,LENNON_STATE_DIR:join(dir,'state'),LENNON_BENCH_MARKS:dir,LENNON_BENCH_EVENTS:'100'});
    const last=p.stdout.trim().split('\n').map(JSON.parse).at(-1);
    if(last.status!=='success'||last.provider!=='codex'||!p.stdout.includes('BENCH_DONE'))throw new Error('Failover failed');
    const gap=Number(BigInt(readFileSync(join(dir,'fallback-first-event'),'utf8'))-BigInt(readFileSync(join(dir,'quota-emitted'),'utf8')))/1e6;
    if(i>=0){pipeline[name].total.push(p.elapsed);pipeline[name].handoff.push(gap);}
    rmSync(dir,{recursive:true,force:true});
  }
  result.cases.failover=Object.fromEntries(Object.entries(pipeline).map(([name,v])=>[name,{totalMs:stats(v.total),handoffMs:stats(v.handoff),totalSamples:v.total,handoffSamples:v.handoff}]));
  const path=resolve(process.argv[2]||join(root,'benchmarks/results/equal-durability.json'));
  writeFileSync(path,JSON.stringify(result,null,2)+'\n');
  console.log('equal-durability failover',JSON.stringify(result.cases.failover));
  console.log(path);
}finally{rmSync(work,{recursive:true,force:true});}
