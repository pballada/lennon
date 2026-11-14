import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync, readFileSync, statSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { Session } from '../src/store.js';
import { invocation, classify, decode } from '../src/providers.js';
import { runProvider } from '../src/runner.js';
import { runTurn } from '../src/harness.js';
import { buildPrompt } from '../src/context.js';

const defaults = () => ({ order: ['claude', 'codex', 'gemini'], cooldownMs: 300000, handoffChars: 120000, providers: {} });
function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'lennon-test-'));
  const session = Session.create(dir, dir);
  session.lock(); t.after(() => { session.unlock(); rmSync(dir, { recursive: true, force: true }); });
  return { dir, session };
}
function mockRun(behaviors, captures = []) {
  return async options => {
    captures.push(options);
    return behaviors[options.name](options);
  };
}
function successful(options) {
  options.onEvent({ type: options.name === 'claude' ? 'system' : 'init', subtype: 'init', session_id: `${options.name}-native` });
  options.onText('Finished'); return { status: 'success' };
}
test('quota failover carries user constraints and completed tool effects', async t => {
  const { session } = fixture(t); const captures = [];
  const result = await runTurn(session, 'Fix build; never deploy', defaults(), { run: mockRun({
    claude: o => {
      o.onEvent({ type: 'assistant', message: { content: [{ type: 'tool_use', id: 't1', name: 'Edit', input: { file: 'app.js' } }] } });
      o.onEvent({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't1', content: 'app.js updated' }] } });
      return { status: 'limit', error: 'rate_limit_error' };
    }, codex: successful,
  }, captures) });
  assert.equal(result.provider, 'codex');
  assert.match(captures[1].prompt, /never deploy/);
  assert.match(captures[1].prompt, /app.js updated/);
  assert.match(captures[1].prompt, /side effects may already/);
  assert.equal(session.meta.pending, false);
  assert.ok(session.meta.cooldowns.claude > Date.now());
});
test('ordered three-provider failover and exhaustion preserves pending task', async t => {
  const { session } = fixture(t); const seen = [];
  const run = async o => { seen.push(o.name); return { status: 'limit', error: 'quota exceeded' }; };
  const result = await runTurn(session, 'Keep going', defaults(), { run });
  assert.equal(result.status, 'exhausted'); assert.deepEqual(seen, ['claude', 'codex', 'gemini']);
  assert.equal(session.meta.pending, true);
  await runTurn(session, undefined, defaults(), { run });
  assert.equal(seen.length, 3, 'cooldowns prevent repeated quota calls');
});
test('auth and permission errors stop without routing to another provider', async t => {
  const { session } = fixture(t); const seen = [];
  const result = await runTurn(session, 'Task', defaults(), { run: async o => { seen.push(o.name); return { status: 'fatal', error: 'Permission denied' }; } });
  assert.equal(result.status, 'fatal'); assert.deepEqual(seen, ['claude']);
});
test('same provider resumes with only new context; returning provider sees intervening work', async t => {
  const { session } = fixture(t); const captures = [];
  const run = mockRun({ claude: successful, codex: successful }, captures);
  await runTurn(session, 'First', defaults(), { run });
  await runTurn(session, 'Second', defaults(), { run });
  assert.ok(captures[1].args.includes('--resume'));
  assert.match(captures[1].prompt, /Second/);
  const config = { ...defaults(), order: ['codex'] };
  await runTurn(session, 'Third from Codex', config, { run });
  await runTurn(session, 'Back to Claude', defaults(), { run });
  assert.match(captures[3].prompt, /Third from Codex/);
  assert.ok(captures[3].args.includes('claude-native'));
});
test('journal recovers native ID and pending state after metadata write is lost', t => {
  const { dir, session } = fixture(t);
  session.append({ type: 'user', text: 'Unfinished' });
  session.append({ type: 'native_session', provider: 'claude', id: 'native-123' });
  session.unlock();
  const recovered = new Session(dir, session.meta.id); recovered.lock();
  try { assert.equal(recovered.meta.pending, true); assert.equal(recovered.meta.providers.claude.id, 'native-123'); }
  finally { recovered.unlock(); }
});
test('journal recovers completed state after metadata write is lost', t => {
  const { dir, session } = fixture(t);
  session.append({ type: 'user', text: 'Done' }); session.append({ type: 'complete', provider: 'codex' }); session.unlock();
  const recovered = new Session(dir, session.meta.id); recovered.lock();
  try { assert.equal(recovered.meta.pending, false); assert.equal(recovered.meta.providers.codex.cursor, 2); }
  finally { recovered.unlock(); }
});
test('exclusive lock rejects concurrent writers and journal repairs torn last record', t => {
  const { dir, session } = fixture(t);
  const other = new Session(dir, session.meta.id);
  assert.throws(() => other.lock(), /already running/);
  session.append({ type: 'user', text: 'Retained' }); session.unlock();
  writeFileSync(session.journal, '{"broken":', { flag: 'a' });
  other.lock();
  try { assert.equal(other.events().length, 1); assert.equal(other.events()[0].text, 'Retained'); }
  finally { other.unlock(); }
});
test('bounded handoff retains full archive and original request', t => {
  const { session } = fixture(t);
  session.append({ type: 'user', text: 'Never deploy' });
  session.append({ type: 'provider_event', provider: 'codex', event: { type: 'item.completed', item: { type: 'command_execution', aggregated_output: 'x'.repeat(20000) } } });
  const prompt = buildPrompt(session, 'gemini', { ...defaults(), handoffChars: 1000 });
  assert.match(prompt, /Never deploy/); assert.match(prompt, /Read the FULL handoff/);
  const path = join(session.dir, 'handoff-gemini.json');
  assert.ok(readFileSync(path, 'utf8').length > 20000);
  if (process.platform !== 'win32') assert.equal(statSync(path).mode & 0o777, 0o600);
});
test('provider argument construction never enables permission bypasses', () => {
  for (const name of ['claude', 'codex', 'gemini']) {
    for (const permission of ['read-only', 'write']) {
      const spec = invocation(name, 'session', defaults(), permission);
      assert.doesNotMatch(spec.args.join(' '), /yolo|dangerously|bypassPermissions/);
      assert.ok(!spec.args.includes('--last'));
    }
  }
  assert.ok(invocation('codex', 'id', defaults()).args.includes('resume'));
  assert.ok(invocation('gemini', 'id', defaults()).args.includes('plan'));
});
test('classification and protocol envelopes distinguish quota from task errors', () => {
  for (const message of ['429', 'rate_limit_error', 'RESOURCE_EXHAUSTED', 'You have hit your limit', 'quota exceeded', 'usage_limit_reached']) assert.equal(classify({ message }), 'limit');
  assert.equal(classify('503 service unavailable'), 'transient');
  assert.equal(classify('Invalid API key'), 'fatal');
  assert.equal(decode('claude', { type: 'assistant', message: { content: [{ type: 'text', text: 'Explain HTTP 429' }] } }).error, undefined);
  assert.equal(decode('codex', { type: 'item.completed', item: { type: 'command_execution', aggregated_output: '429' } }).error, undefined);
});

function executable(dir, source) {
  const path = join(dir, 'fake-provider');
  writeFileSync(path, `#!${process.execPath}\n${source}`, { mode: 0o755 }); return path;
}
test('real child streams fragmented UTF-8, preserves stdin, strips API key env, completes', async t => {
  const { dir } = fixture(t);
  const command = executable(dir, `let p=''; for await (const c of process.stdin) p+=c;
    if(p!=='hello $()') process.exit(2);
    if(process.env.OPENAI_API_KEY) process.exit(3);
    const b=Buffer.from(JSON.stringify({type:'item.completed',item:{type:'agent_message',text:'🐕 done'}})+'\\n');
    for(const x of b) process.stdout.write(Buffer.from([x]));
    console.log(JSON.stringify({type:'turn.completed'}));`);
  let text = '';
  const result = await runProvider({ name: 'codex', command, args: [], cwd: dir, prompt: 'hello $()', environment: { ...process.env, OPENAI_API_KEY: 'must-not-leak' }, onEvent: () => {}, onText: s => text += s });
  assert.equal(result.status, 'success'); assert.equal(text, '🐕 done');
});
test('quota envelope exits upstream retry loop quickly', async t => {
  const { dir } = fixture(t);
  const command = executable(dir, `console.log(JSON.stringify({type:'error',message:'rate_limit_error 429'})); setInterval(()=>{},10000);`);
  const start = Date.now();
  const result = await runProvider({ name: 'codex', command, args: [], cwd: dir, prompt: '', onEvent: () => {}, onText: () => {} });
  assert.equal(result.status, 'limit'); assert.ok(Date.now() - start < 4000);
});
test('cancellation and timeout stop children and never classify as success', async t => {
  const { dir } = fixture(t);
  const command = executable(dir, 'setInterval(()=>{},10000);');
  const controller = new AbortController(); setTimeout(() => controller.abort(), 50);
  const args = { name: 'claude', command, args: [], cwd: dir, prompt: '', onEvent: () => {}, onText: () => {} };
  assert.equal((await runProvider({ ...args, signal: controller.signal })).status, 'cancelled');
  assert.equal((await runProvider({ ...args, timeoutMs: 50 })).status, 'timeout');
});
test('spawn failure is missing, and exit zero without terminal event is fatal', async t => {
  const { dir } = fixture(t);
  const args = { name: 'codex', args: [], cwd: dir, prompt: '', onEvent: () => {}, onText: () => {} };
  assert.equal((await runProvider({ ...args, command: join(dir, 'absent') })).status, 'missing');
  const command = executable(dir, 'console.log("not protocol output");');
  assert.equal((await runProvider({ ...args, command })).status, 'fatal');
});
test('successful completion supersedes recovered nonquota error events', async t => {
  const { dir } = fixture(t);
  const command = executable(dir, `console.log(JSON.stringify({type:'error',message:'Temporary connection failure'})); console.log(JSON.stringify({type:'turn.completed'}));`);
  const result = await runProvider({ name: 'codex', command, args: [], cwd: dir, prompt: '', onEvent: () => {}, onText: () => {} });
  assert.equal(result.status, 'success');
});
test('Gemini stream warning, tool activity, native session and final outcome', async t => {
  const { dir } = fixture(t);
  const command = executable(dir, `for await(const c of process.stdin){};
    for(const e of [
      {type:'init',session_id:'gemini-session'},
      {type:'error',severity:'warning',message:'Nonfatal warning'},
      {type:'tool_use',tool_name:'read_file',parameters:{file_path:'README.md'}},
      {type:'tool_result',status:'success',output:'Project constraints'},
      {type:'message',role:'assistant',content:'Done',delta:true},
      {type:'result',status:'success'}
    ]) console.log(JSON.stringify(e));`);
  const events = []; let text = '';
  const result = await runProvider({ name: 'gemini', command, args: [], cwd: dir, prompt: 'Task', onEvent: e => events.push(e), onText: s => text += s });
  assert.equal(result.status, 'success'); assert.equal(result.nativeId, 'gemini-session');
  assert.equal(text, 'Done'); assert.equal(events[3].output, 'Project constraints');
});
test('stale lock recovery and malformed journal do not leave owned locks behind', t => {
  const { dir, session } = fixture(t);
  session.unlock();
  const lock = join(session.dir, 'lock');
  writeFileSync(lock, '2147483647');
  const recovered = new Session(dir, session.meta.id); recovered.lock(); recovered.unlock();
  writeFileSync(session.journal, 'not-json\n');
  assert.throws(() => recovered.lock(), /JSON|Unexpected/);
  assert.equal(existsSync(lock), false);
});
test('CLI end-to-end failover, resume, JSONL and private state', t => {
  const { dir } = fixture(t);
  const claude = join(dir, 'mock-claude'), codex = join(dir, 'mock-codex');
  writeFileSync(claude, `#!${process.execPath}\nconsole.log(JSON.stringify({type:'result',is_error:true,subtype:'error_during_execution',result:'rate_limit_error'}));`, { mode: 0o755 });
  writeFileSync(codex, `#!${process.execPath}\nlet p=''; for await(const c of process.stdin)p+=c; if(!p.includes('never deploy'))process.exit(4); console.log(JSON.stringify({type:'thread.started',thread_id:'test-thread'})); console.log(JSON.stringify({type:'item.completed',item:{type:'agent_message',text:'All done'}})); console.log(JSON.stringify({type:'turn.completed'}));`, { mode: 0o755 });
  const configPath = join(dir, 'config.json');
  writeFileSync(configPath, JSON.stringify({ order: ['claude','codex'], providers: { claude: { command: claude }, codex: { command: codex } } }));
  const env = { ...process.env, LENNON_CONFIG: configPath, LENNON_STATE_DIR: join(dir, 'state') };
  const cli = new URL('../bin/lennon.js', import.meta.url).pathname;
  const first = spawnSync(process.execPath, [cli, '--json', '-p', 'Fix tests, never deploy', '--cwd', dir], { env, encoding: 'utf8' });
  assert.equal(first.status, 0, first.stderr + first.stdout);
  const events = first.stdout.trim().split('\n').map(JSON.parse);
  assert.equal(events.at(-1).provider, 'codex');
  const id = events[0].session;
  const second = spawnSync(process.execPath, [cli, '--json', '--resume', id, '-p', 'Continue; never deploy'], { env, encoding: 'utf8' });
  assert.equal(second.status, 0, second.stderr);
  assert.match(second.stdout, /resuming/);
  assert.ok(existsSync(join(dir, 'state', 'sessions', id, 'events.jsonl')));
});
