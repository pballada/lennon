import { readFileSync, openSync, writeSync, fsyncSync, closeSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
const sync = process.env.LENNON_BENCH_FULLSYNC ? (await import('./fullsync.mjs')).sync : fsyncSync;

if (process.argv[2] === '--help') {
  console.log('lennon benchmark probe');
} else {
  const [mode, input, output] = process.argv.slice(2);
  const start = performance.now();
  const source = readFileSync(input, 'utf8');
  let events = 0, bytes = 0, textBytes = 0, tools = 0, limits = 0;
  const persistent = mode === 'journal-open' ? openSync(output, 'a', 0o600) : undefined;
  try {
    for (const line of source.split('\n')) {
      if (!line) continue;
      const event = JSON.parse(line);
      events++;
      if (event.event?.delta?.text) textBytes += Buffer.byteLength(event.event.delta.text);
      if (event.type === 'assistant') tools++;
      if (event.type === 'error' && event.message?.includes('rate_limit')) limits++;
      const serialized = JSON.stringify({ time: '2026-11-14T00:00:00.000Z', type: 'provider_event', provider: 'claude', event }) + '\n';
      bytes += Buffer.byteLength(serialized);
      if (mode === 'journal' || persistent !== undefined) {
        const fd = persistent ?? openSync(output, 'a', 0o600);
        try { writeSync(fd, serialized); sync(fd); }
        finally { if (persistent === undefined) closeSync(fd); }
      }
    }
  } finally { if (persistent !== undefined) closeSync(persistent); }
  console.log(JSON.stringify({ events, bytes, textBytes, tools, limits, workMs: performance.now() - start }));
}
