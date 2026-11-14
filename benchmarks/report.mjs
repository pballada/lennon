import { readFileSync, writeFileSync } from 'node:fs';
import { resolve, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const source = resolve(process.argv[2] || join(here,'results/local.json'));
const output = resolve(process.argv[3] || join(here,'RESULTS.md'));
const r = JSON.parse(readFileSync(source,'utf8'));
const equalSource = resolve(process.argv[4] || join(here,'results/equal-durability.json'));
const equal = JSON.parse(readFileSync(equalSource,'utf8'));
const f = n => n.toFixed(2);
function q(v,p) { const a=[...v].sort((x,y)=>x-y); const i=(a.length-1)*p; return a[Math.floor(i)]+(a[Math.ceil(i)]-a[Math.floor(i)])*(i%1); }
let seed = 0x6c656e6e;
function rand(n) { seed = (Math.imul(seed,1664525)+1013904223)>>>0; return Math.floor(seed / 2 ** 32*n); }
function interval(a,b) {
  const differences=a.map((v,i)=>v-b[i]), medians=[];
  for(let i=0;i<10000;i++) medians.push(q(differences.map(()=>differences[rand(differences.length)]),.5));
  return `${f(q(differences,.5))} ms [${f(q(medians,.025))}, ${f(q(medians,.975))}]`;
}
const rows=[];
for(const [key,label] of [['startup','Fresh-process startup'],['json_100k','100k JSON events'],['durable_250','250 durable journal events']]) {
  const c=r.cases[key];
  if(key!=='durable_250') for(const [name,v] of Object.entries(c.summary)) rows.push(`| ${label} | ${name} | ${f(v.wallMs.median)} | ${f(v.wallMs.p95)} | ${v.workMs ? f(v.workMs.median) : '—'} |`);
}
const pipelines=Object.entries(r.cases.actual_lennon_failover_100_events);
const pipelineRows=pipelines.map(([name,v])=>`| ${name} | ${f(v.totalMs.median)} | ${f(v.totalMs.p95)} | ${f(v.handoffMs.median)} | ${f(v.handoffMs.p95)} |`);
const memoryRows=Object.entries(r.memory).map(([name,v])=>`| ${name} | ${f(v.peakRssMiB.median)} |`);
const equalRows=Object.entries(equal.cases.journal_100).map(([name,v])=>`| ${name} | ${f(v.wallMs.median)} | ${f(v.wallMs.p95)} | ${f(v.workMs.median)} |`);
const equalPipeline=Object.entries(equal.cases.failover).map(([name,v])=>`| ${name} | ${f(v.totalMs.median)} | ${f(v.totalMs.p95)} | ${f(v.handoffMs.median)} | ${f(v.handoffMs.p95)} |`);
const intervals=[];
for(const [key,label] of [['startup','Probe startup'],['json_100k','100k JSON']]) {
  const c=r.cases[key]; const suffix=key==='startup'?'probe':key==='json_100k'?'parse':'journal';
  const a=c.samples[`node_${suffix}`];
  for(const b of ['rust','bun']) if(c.samples[`${b}_${suffix}`]) intervals.push(`| ${label}: Node minus ${b} | ${interval(a,c.samples[`${b}_${suffix}`])} |`);
}
const full=r.cases.startup;
if(full.samples.lennon_bun) intervals.push(`| Full Lennon startup: Node minus Bun | ${interval(full.samples.lennon_full,full.samples.lennon_bun)} |`);
intervals.push(`| Equal full-sync journal: Node minus Rust | ${interval(equal.cases.journal_100.node_journal.samples,equal.cases.journal_100.rust_journal.samples)} |`);
intervals.push(`| Equal full-sync journal: Node minus Bun | ${interval(equal.cases.journal_100.node_journal.samples,equal.cases.journal_100.bun_fullsync_journal.samples)} |`);
intervals.push(`| Equal full-sync handoff: Node minus Bun | ${interval(equal.cases.failover.node.handoffSamples,equal.cases.failover.bun_fullsync.handoffSamples)} |`);
const text=`# Runtime measurements · ${r.generated.slice(0,10)}

Measured on ${r.environment.cpu}, ${r.environment.platform}/${r.environment.arch},
OS kernel ${r.environment.osRelease}. Node ${r.environment.node}; Bun ${r.environment.bun};
${r.environment.rust}. Release Rust build with LTO.

## Controlled results

Times are milliseconds; smaller is faster. Wall time includes a fresh process.
Work time starts inside the process and excludes runtime startup. Each row uses
five warmups and balanced randomized samples, with no outlier removal.

| Workload | Implementation | Wall median | Wall p95 | Work median |
| --- | --- | ---: | ---: | ---: |
${rows.join('\n')}

"probe" variants implement the same tiny help task. Full Lennon help loads the
actual production modules. Rust's JSON/journal probe implements the measured
hot path, not the rest of Lennon. It is not a feature-equivalent Rust product.

Every journal candidate requested fsync after every event, and all journal
SHA-256 digests and event counters matched. However, the underlying operations
were different: macOS Node and Rust request F_FULLFSYNC, while Bun 1.4.4 calls
plain fsync. Initial Bun journaling results are **not equal-durability wins**.
The journal-open variant retains the descriptor and still syncs every event.

## Corrected comparison: identical macOS full-sync policy

The follow-up explicitly uses F_FULLFSYNC in each runtime after every event.
Its Bun shim is benchmark-only; no production source or default runtime changed.
Journal output hashes match. Fifteen samples per journal variant, five warmups.

| 100-event journal | Wall median | Wall p95 | Work median |
| --- | ---: | ---: | ---: |
${equalRows.join('\n')}

For the complete pipeline, the Bun overlay changes only the imported sync
binding in a temporary copy of Lennon. Twenty samples per runtime, five warmups.

| Equal-durability runtime | Total median | Total p95 | Handoff median | Handoff p95 |
| --- | ---: | ---: | ---: | ---: |
${equalPipeline.join('\n')}

## Actual Lennon automatic failover

The initial, uncorrected runs use the shipping harness, native mock providers, 100 streamed
primary events, an explicit quota failure, and successful Codex completion.
The fallback checks that the user request and streamed context reached it.
Handoff measures quota emission until the next provider has read the full prompt.

| Runtime | Total median | Total p95 | Handoff median | Handoff p95 |
| --- | ---: | ---: | ---: | ---: |
${pipelineRows.join('\n')}

These initial Bun timings include weaker disk flushes and are not comparable
at the same durability. Use the corrected results above for that comparison.
No equivalent full Rust harness has been implemented or measured.

## Memory

Peak RSS in MiB, median of five separate macOS time(1) samples. Parse probes
process the same 100,000-event file. This is not a long-running session memory test.

| Variant | Peak RSS median |
| --- | ---: |
${memoryRows.join('\n')}

## Uncertainty

Paired median differences with a 95% percentile bootstrap interval (10,000
resamples). A positive value means the second candidate is faster in this
experiment. This estimates sampling variation, not performance on other machines.

| Comparison | Median difference [95% bootstrap interval] |
| --- | ---: |
${intervals.join('\n')}

## Decision

Rust wins the minimal startup test. That alone does not prove a Rust rewrite
would deliver the fastest complete coding harness. JSON processing, journaling,
and provider lifecycle behavior need separate evidence, as shown above.

The primary measured streaming bottleneck is synchronous full-disk flushing
per event. Changing the language does not remove that cost. Descriptor reuse
alone does not remove it either. Investigate journal flush scheduling before
committing to a full rewrite; any batching needs an explicit durability contract.

Bun wins this JSON workload and speeds up full CLI startup without rewriting
the application. It passed the 18 mock/subprocess tests on this machine, but
its default macOS flush behavior differs. Keep Node as the supported default
until durability, live providers, and cross-platform behavior are validated.
Rust is the strongest measured candidate for minimal startup and low memory,
not a demonstrated full-harness winner. A full Rust port still needs session,
permissions, streaming, and recovery parity validation.

Do not extrapolate mock model timing to live provider/model latency. No real
quota exhaustion or live network generation was used in this experiment.
The findings apply to these candidates and this machine; they are not proof
of a universally fastest implementation.

Reproduction and limitations: [README.md](README.md). Raw samples, versions,
digests, counters, and distributions: [initial run](results/${source.split('/').at(-1)})
and [equal-durability follow-up](results/${equalSource.split('/').at(-1)}).

The runtime source evidence explaining the sync mismatch is linked in
[the methodology](README.md#workloads).
`;
writeFileSync(output,text);
console.log(output);
