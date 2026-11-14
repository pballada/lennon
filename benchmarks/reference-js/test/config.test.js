import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { normalizeConfig, applyOverrides, loadConfig } from '../src/config.js';
import { editConfig } from '../src/config-command.js';
import { invocation } from '../src/providers.js';
import { Session } from '../src/store.js';
import { runTurn } from '../src/harness.js';

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), 'lennon-config-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const env = { ...process.env, LENNON_CONFIG: join(dir, 'config.json'), LENNON_STATE_DIR: join(dir, 'state') };
  return { dir, env };
}
test('defaults and legacy model/command/order config remain compatible', () => {
  const c = normalizeConfig({ order: ['gemini', 'claude'], providers: { claude: { model: 'sonnet' } } });
  assert.deepEqual(c.order, ['gemini', 'claude']);
  assert.equal(c.providers.claude.adapter, 'claude');
  assert.ok(invocation('claude', undefined, c).args.includes('sonnet'));
});
test('named entries select adapter, model and executable independently', () => {
  const c = normalizeConfig({ order: ['fast', 'deep'], providers: {
    fast: { adapter: 'codex', model: 'model-a', command: '/my/codex' },
    deep: { adapter: 'codex', model: 'model-b' },
  } });
  const fast = invocation('fast', 'fast-thread', c), deep = invocation('deep', undefined, c);
  assert.equal(fast.command, '/my/codex'); assert.ok(fast.args.includes('model-a'));
  assert.ok(fast.args.includes('fast-thread')); assert.ok(deep.args.includes('model-b'));
});
test('CLI overrides reorder and select models without mutating config', () => {
  const base = normalizeConfig();
  const c = applyOverrides(base, { order: ' codex, claude ', models: ['codex=model-a', 'claude=sonnet'] });
  assert.deepEqual(c.order, ['codex', 'claude']); assert.equal(c.providers.codex.model, 'model-a');
  assert.deepEqual(base.order, ['claude', 'codex', 'gemini']); assert.equal(base.providers.codex.model, undefined);
  assert.deepEqual(applyOverrides(base, { provider: 'gemini' }).order, ['gemini']);
  assert.throws(() => applyOverrides(base, { provider: 'gemini', order: 'claude' }), /Choose/);
  for (const models of [['unknown=x'], ['claude='], ['sonnet'], ['claude=a', 'claude=b']]) assert.throws(() => applyOverrides(base, { models }), /--model/);
});
test('invalid configs fail early with actionable messages', () => {
  for (const user of [null, [], { providers: { claude: null } }, { providers: { fast: { model: 'a' } } },
    { providers: { claude: { enabled: 'false' } } }, { order: ['missing'] }, { order: ['claude', 'claude'] },
    { providers: { claude: { model: '' } } }, { providers: { claude: { model: '-h' } } },
    { providers: { claude: { modle: 'sonnet' } } }, { oder: ['claude'] },
    { order: ['claude'], providers: { claude: { enabled: false } } },
    JSON.parse('{"providers":{"__proto__":{"adapter":"codex"}}}')]) assert.throws(() => normalizeConfig(user));
});
test('config init, typed set and unset persist settings without clobbering on invalid edits', t => {
  const { env } = fixture(t);
  editConfig(['init'], env);
  assert.throws(() => editConfig(['init'], env), /EEXIST/);
  editConfig(['set', 'order', 'codex,claude'], env);
  editConfig(['set', 'providers.claude.model', 'sonnet'], env);
  editConfig(['set', 'providers.gemini.enabled', 'false'], env);
  editConfig(['set', 'cooldownMs', '1000'], env);
  let c = loadConfig(env);
  assert.deepEqual(c.order, ['codex', 'claude']); assert.equal(c.providers.gemini.enabled, false);
  assert.equal(c.cooldownMs, 1000); assert.equal(c.providers.claude.model, 'sonnet');
  const before = readFileSync(env.LENNON_CONFIG, 'utf8');
  assert.throws(() => editConfig(['set', 'order', 'unknown'], env));
  assert.throws(() => editConfig(['set', 'providers.__proto__.adapter', 'codex'], env));
  assert.equal(readFileSync(env.LENNON_CONFIG, 'utf8'), before);
  editConfig(['unset', 'providers.claude.model'], env);
  assert.equal(loadConfig(env).providers.claude.model, undefined);
});
test('config can add a custom model entry atomically', t => {
  const { env } = fixture(t);
  editConfig(['set', 'providers.fast', '{"adapter":"claude","model":"sonnet"}'], env);
  editConfig(['set', 'order', 'fast,codex'], env);
  assert.equal(loadConfig(env).providers.fast.model, 'sonnet');
});
test('disabled entries are skipped and configured route order is honored', async t => {
  const { dir } = fixture(t); const session = Session.create(dir, dir); session.lock();
  try {
    const c = normalizeConfig({ order: ['disabled', 'fast', 'deep'], providers: {
      disabled: { adapter: 'gemini', enabled: false }, fast: { adapter: 'claude', model: 'sonnet' }, deep: { adapter: 'codex', model: 'model-b' },
    } });
    const seen = [];
    const result = await runTurn(session, 'Keep the constraints', c, { run: async o => {
      seen.push(o.name);
      if (o.name === 'claude') {
        assert.ok(o.args.includes('sonnet'));
        o.onEvent({ type: 'assistant', message: { content: [{ type: 'text', text: 'Remember my finding' }] } });
        return { status: 'limit', error: 'rate_limit_error' };
      }
      assert.match(o.prompt, /Remember my finding/); assert.ok(o.args.includes('model-b'));
      return { status: 'success' };
    } });
    assert.deepEqual(seen, ['claude', 'codex']); assert.equal(result.provider, 'deep');
  } finally { session.unlock(); }
});
test('two models on the same adapter keep independent native histories', async t => {
  const { dir } = fixture(t); const session = Session.create(dir, dir); session.lock();
  try {
    const c = normalizeConfig({ order: ['fast', 'deep'], providers: { fast: { adapter: 'codex', model: 'a' }, deep: { adapter: 'codex', model: 'b' } } });
    await runTurn(session, 'Task', c, { run: async o => {
      const model = o.args[o.args.indexOf('--model') + 1];
      o.onEvent({ type: 'thread.started', thread_id: `thread-${model}` });
      return model === 'a' ? { status: 'limit', error: 'quota' } : { status: 'success' };
    } });
    assert.equal(session.meta.providers.fast.id, 'thread-a'); assert.equal(session.meta.providers.deep.id, 'thread-b');
    assert.ok(session.meta.cooldowns.fast); assert.equal(session.meta.cooldowns.deep, undefined);
  } finally { session.unlock(); }
});
test('changing model starts fresh, replays old context, and survives journal recovery', async t => {
  const { dir } = fixture(t); const session = Session.create(dir, dir); session.lock();
  const config = model => normalizeConfig({ order: ['codex'], providers: { codex: { model } } });
  try {
    await runTurn(session, 'Original constraint', config('a'), { run: async o => {
      o.onEvent({ type: 'thread.started', thread_id: 'old-thread' });
      o.onEvent({ type: 'item.completed', item: { type: 'agent_message', text: 'Important result from model a' } });
      return { status: 'success' };
    } });
    await runTurn(session, 'Continue with b', config('b'), { run: async o => {
      assert.ok(!o.args.includes('resume')); assert.match(o.prompt, /Important result from model a/);
      o.onEvent({ type: 'thread.started', thread_id: 'new-thread' }); return { status: 'success' };
    } });
  } finally { session.unlock(); }
  const recovered = new Session(dir, session.meta.id); recovered.lock();
  try {
    assert.equal(recovered.meta.providers.codex.id, 'new-thread'); assert.equal(recovered.meta.providers.codex.route.model, 'b');
    await runTurn(recovered, 'Another turn', config('b'), { run: async o => {
      assert.ok(o.args.includes('resume')); assert.ok(o.args.includes('new-thread')); return { status: 'success' };
    } });
  } finally { recovered.unlock(); }
});
test('changing a quota-limited model clears its stale cooldown even before a native ID arrives', async t => {
  const { dir } = fixture(t); const session = Session.create(dir, dir); session.lock();
  try {
    const c = model => normalizeConfig({ order: ['claude'], providers: { claude: { model } } });
    await runTurn(session, 'Task', c('a'), { run: async () => ({ status: 'limit', error: 'quota' }) });
    let called = false;
    const result = await runTurn(session, undefined, c('b'), { run: async () => { called = true; return { status: 'success' }; } });
    assert.equal(called, true); assert.equal(result.status, 'success');
  } finally { session.unlock(); }
});
test('CLI config commands and flags reach a real subprocess', t => {
  const { dir, env } = fixture(t);
  const cli = new URL('../bin/lennon.js', import.meta.url).pathname;
  const call = args => spawnSync(process.execPath, [cli, ...args], { env, encoding: 'utf8', input: '' });
  assert.equal(call(['config', 'init']).status, 0);
  const provider = join(dir, 'fake-codex');
  writeFileSync(provider, `#!${process.execPath}\nlet text='';for await(const c of process.stdin)text+=c;const a=process.argv.slice(2);if(a[a.indexOf('--model')+1]!=='override-model')process.exit(9);console.log(JSON.stringify({type:'item.completed',item:{type:'agent_message',text:'Configured model reached provider'}}));console.log(JSON.stringify({type:'turn.completed'}));`, { mode: 0o755 });
  assert.equal(call(['config', 'set', 'providers.codex.command', provider]).status, 0);
  assert.equal(call(['config', 'set', 'providers.codex.model', 'saved-model']).status, 0);
  const run = call(['--provider', 'codex', '--model', 'codex=override-model', '--json', '--cwd', dir, '-p', 'Task']);
  assert.equal(run.status, 0, run.stdout + run.stderr); assert.match(run.stdout, /Configured model reached provider/);
  assert.equal(loadConfig(env).providers.codex.model, 'saved-model');
  const show = call(['--order', 'gemini,codex', '--model', 'gemini=custom', 'config']);
  assert.deepEqual(JSON.parse(show.stdout).order, ['gemini', 'codex']);
  assert.equal(JSON.parse(show.stdout).providers.gemini.model, 'custom');
  const missing = join(dir, 'missing.json');
  assert.notEqual(call(['--config', missing, 'config']).status, 0);
  assert.equal(existsSync(missing), false);
});
