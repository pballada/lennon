import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, statSync } from 'node:fs';
import { tmpdir, arch, platform, release, cpus } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { performance } from 'node:perf_hooks';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const reference = join(root, 'benchmarks/reference-js');
const rust = process.env.LENNON_BENCH_RUST || join(root, 'benchmarks/rust/target/release/lennon-perf-probe');
const bun = process.env.LENNON_BENCH_BUN;
const runtimes = [['node', process.execPath], ...(bun ? [['bun', bun]] : [])];
const artifact = resolve(process.argv[2] || join(root, 'benchmarks/results/local.json'));
const work = mkdtempSync(join(tmpdir(), 'lennon-bench-'));
const counters = ['events', 'bytes', 'textBytes', 'tools', 'limits'];
let rng = 0x6c656e6e;
function random() { rng = (Math.imul(rng, 1664525) + 1013904223) >>> 0; return rng / 2 ** 32; }
function shuffle(a) { for (let i = a.length - 1; i > 0; i--) { const j = Math.floor(random() * (i + 1)); [a[i], a[j]] = [a[j], a[i]]; } return a; }
function quantile(v, q) { const a = [...v].sort((x,y) => x-y); const i = (a.length - 1) * q; return a[Math.floor(i)] + (a[Math.ceil(i)] - a[Math.floor(i)]) * (i % 1); }
function stats(v) { return { n: v.length, median: quantile(v,.5), p95: quantile(v,.95), min: Math.min(...v), max: Math.max(...v) }; }
function execute(cmd, args, options = {}) {
  const start = performance.now();
  const result = spawnSync(cmd, args, { cwd: root, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, ...options });
  const elapsed = performance.now() - start;
  if (result.error || result.status !== 0) throw new Error(`${cmd} failed: ${result.error?.message || result.stderr || result.stdout}`);
  return { elapsed, stdout: result.stdout, stderr: result.stderr };
}
function inputFile(n) {
  const path = join(work, `events-${n}.jsonl`);
  const rows = [];
  for (let i = 0; i < n; i++) {
    const event = i % 100 === 99 ? { type: 'error', message: 'rate_limit_error 429' } : i % 10 === 9 ?
      { type: 'assistant', message: { content: [{ type: 'tool_use', id: `tool-${i}`, name: 'Read', input: { path: `src/file-${i}.js` } }] } } :
      { type: 'stream_event', event: { type: 'content_block_delta', delta: { type: 'text_delta', text: `hello 🐕 event ${i} ${'x'.repeat(80)}` } } };
    rows.push(JSON.stringify(event));
  }
  writeFileSync(path, rows.join('\n') + '\n'); return path;
}
const results = { generated: new Date().toISOString(), environment: { platform: platform(), arch: arch(), osRelease: release(), cpu: cpus()[0]?.model, node: process.version, bun: bun ? execute(bun,['--version']).stdout.trim() : null, rust: process.env.LENNON_BENCH_RUST_VERSION || 'see build log' }, methodology: { warmups: 5, seed: '0x6c656e6e', sampling: 'balanced randomized blocks; fresh process per sample; warm filesystem caches; no live models', journal: 'open/write/fsync/close per event, or persistent descriptor/write/fsync per event; no durability weakening', limitations: 'Rust is a matched hot-path probe, not a full Lennon port. Cold disk caches, live providers, and network latency are not measured.' }, cases: {}, validation: {} };
function sample(label, variants, count, validate = () => {}) {
  const samples = Object.fromEntries(Object.keys(variants).map(k => [k, []]));
  const workSamples = Object.fromEntries(Object.keys(variants).map(k => [k, []]));
  for (let iteration = -5; iteration < count; iteration++) {
    for (const name of shuffle(Object.keys(variants))) {
      const value = variants[name](); validate(value, name);
      if (iteration >= 0) {
        samples[name].push(value.elapsed);
        if (value.workMs !== undefined) workSamples[name].push(value.workMs);
      }
    }
  }
  const summary = Object.fromEntries(Object.entries(samples).map(([name, values]) => [name, { wallMs: stats(values), ...(workSamples[name].length ? { workMs: stats(workSamples[name]) } : {}) }]));
  results.cases[label] = { summary, samples, workSamples };
  console.log(label, JSON.stringify(summary));
}
try {
  const nodeProbe = [join(root, 'benchmarks/probe.mjs')];
  sample('startup', {
    node_probe: () => execute(process.execPath, [...nodeProbe, '--help']),
    rust_probe: () => execute(rust, ['--help']),
    lennon_full: () => execute(process.execPath, [join(reference,'bin/lennon.js'), '--help']),
    ...(bun ? {
      bun_probe: () => execute(bun, [...nodeProbe, '--help']),
      lennon_bun: () => execute(bun, [join(reference,'bin/lennon.js'),'--help']),
    } : {}),
  }, 100);

  for (const [label, n, modes, samples] of [
    ['json_100k', 100000, ['parse'], 25],
    ['durable_250', 250, ['journal', 'journal-open'], 20],
  ]) {
    const source = inputFile(n);
    const variants = {};
    let expected;
    const digests = {};
    for (const mode of modes) for (const language of [...runtimes.map(([name]) => name), 'rust']) {
      const name = `${language}_${mode}`;
      variants[name] = () => {
        const output = join(work, `${name}.jsonl`);
        rmSync(output, { force: true });
        const runtime = runtimes.find(([name]) => name === language)?.[1];
        const value = runtime ? execute(runtime, [...nodeProbe, mode, source, output]) : execute(rust, [mode, source, output]);
        const parsed = JSON.parse(value.stdout);
        const counts = Object.fromEntries(counters.map(k => [k, parsed[k]]));
        if (!expected) expected = JSON.stringify(counts);
        if (JSON.stringify(counts) !== expected) throw new Error(`Mismatched results: ${name}`);
        if (mode !== 'parse') {
          const digest = createHash('sha256').update(readFileSync(output)).digest('hex');
          digests[name] = digest;
          if (new Set(Object.values(digests)).size !== 1) throw new Error('Journals differ');
        }
        return { ...value, workMs: parsed.workMs };
      };
    }
    sample(label, variants, samples);
    results.validation[label] = { counters: JSON.parse(expected), digests, inputBytes: statSync(source).size };
  }

  // Measure the shipping harness separately, using the same native mock providers.
  // Mock timestamps measure quota-emitted -> fallback receives complete prompt.
  const primary = join(work, 'provider-claude'), fallback = join(work, 'provider-codex');
  symlinkSync(rust, primary); symlinkSync(rust, fallback);
  const config = join(work, 'config.json');
  writeFileSync(config, JSON.stringify({ order: ['claude','codex'], providers: { claude: { command: primary }, codex: { command: fallback } } }));
  const pipeline = Object.fromEntries(runtimes.map(([name]) => [name,{ handoff: [], total: [] }]));
  for (let i = -5; i < 30; i++) {
    for (const [name, runtime] of shuffle([...runtimes])) {
    const dir = mkdtempSync(join(work, 'pipeline-'));
    const run = execute(runtime, [join(reference,'bin/lennon.js'), '--json', '--cwd', root, '-p', 'benchmark task: finish the existing work'], {
      env: { ...process.env, LENNON_CONFIG: config, LENNON_STATE_DIR: join(dir,'state'), LENNON_BENCH_MARKS: dir, LENNON_BENCH_EVENTS: '100' },
    });
    const output = run.stdout.trim().split('\n').map(JSON.parse);
    const last = output.at(-1);
    if (last.status !== 'success' || last.provider !== 'codex' || !run.stdout.includes('BENCH_DONE')) throw new Error('Actual Lennon failover failed');
    const gap = Number(BigInt(readFileSync(join(dir, 'fallback-first-event'), 'utf8')) - BigInt(readFileSync(join(dir, 'quota-emitted'), 'utf8'))) / 1e6;
    if (i >= 0) { pipeline[name].handoff.push(gap); pipeline[name].total.push(run.elapsed); }
    rmSync(dir, { recursive: true, force: true });
    }
  }
  results.cases.actual_lennon_failover_100_events = Object.fromEntries(Object.entries(pipeline).map(([name,v]) => [name,{ handoffMs: stats(v.handoff), totalMs: stats(v.total), handoffSamples:v.handoff, totalSamples:v.total }]));
  console.log('actual_lennon_failover_100_events', JSON.stringify(results.cases.actual_lennon_failover_100_events));

  // Peak RSS is sampled separately; time(1) overhead is excluded from latency data.
  const source = inputFile(100000);
  results.memory = {};
  if (platform() === 'darwin') {
    for (const [name, cmd, args] of [
      ['node_probe', process.execPath, [...nodeProbe, 'parse', source, join(work,'ignore')]],
      ['rust_probe', rust, ['parse', source, join(work,'ignore')]],
      ['lennon_help', process.execPath, [join(reference,'bin/lennon.js'),'--help']],
      ...(bun ? [['bun_probe',bun,[...nodeProbe,'parse',source,join(work,'ignore')]]] : []),
    ]) {
      const memory = [];
      for (let i = 0; i < 5; i++) {
        const value = execute('/usr/bin/time', ['-l', cmd, ...args]);
        const match = value.stderr.match(/(\d+)\s+maximum resident set size/);
        if (!match) throw new Error('Cannot parse RSS');
        memory.push(Number(match[1]) / 1024 / 1024);
      }
      results.memory[name] = { peakRssMiB: stats(memory) };
    }
  }
  mkdirSync(dirname(artifact), { recursive: true });
  writeFileSync(artifact, JSON.stringify(results, null, 2) + '\n');
  console.log(`Raw results: ${artifact}`);
} finally { rmSync(work, { recursive: true, force: true }); }
