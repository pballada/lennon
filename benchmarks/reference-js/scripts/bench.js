import { spawnSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
const samples = [];
for (let i = 0; i < 20; i++) {
  const start = performance.now();
  const result = spawnSync(process.execPath, ['bin/lennon.js', '--help'], { stdio: 'ignore' });
  if (result.status !== 0) throw new Error('CLI failed');
  samples.push(performance.now() - start);
}
samples.sort((a,b) => a-b);
console.log(`CLI startup (20 fresh processes): median ${samples[10].toFixed(1)}ms, p95 ${samples[18].toFixed(1)}ms`);
console.log('Measures Lennon startup only; provider startup and model latency are separate.');
