#!/usr/bin/env node
import { parseArgs } from 'node:util';
import { createInterface } from 'node:readline/promises';
import { stdin, stdout, stderr } from 'node:process';
import { realpathSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { loadConfig, stateRoot, configPath, applyOverrides, adapterFor } from '../src/config.js';
import { Session, listSessions } from '../src/store.js';
import { runTurn } from '../src/harness.js';

const help = `lennon · the loyal coding harness

Usage: lennon [options] [prompt]
       lennon doctor | sessions | providers | config | version

  -p, --prompt TEXT      Run one task, then exit (also accepts piped stdin)
  -r, --resume ID        Resume a Lennon session; no prompt retries unfinished work
  -c, --continue         Resume latest session in this working directory
      --cwd PATH        Project directory (default: current directory)
      --order LIST      Provider order, e.g. claude,codex,gemini
      --provider NAME   Use only this configured provider entry
      --model ID=MODEL  Override an entry's model (repeatable)
      --config PATH     Read a specific config file
      --write           Allow edits using each CLI's native permission policy
      --timeout MS      Stop a provider after MS; no automatic retry of timed-out work
      --retry           Clear this session's provider cooldowns
      --json            Emit Lennon events as JSONL
  -h, --help            Show help

With no prompt, starts a conversation. /status, /help, /exit are available.
Read-only tool policy is the default. --write permits edits; it never bypasses
the upstream sandbox or enables yolo mode. Login with each native CLI first.
Provider switchovers share observable history and files, not private model state.

Configuration:
  lennon config init
  lennon config set order codex,claude,gemini
  lennon config set providers.claude.model sonnet
  lennon config set providers.gemini.enabled false
  lennon config unset providers.claude.model
`;
function print(value) { stdout.write(value + '\n'); }
function safeText(value) {
  // Strip terminal controls from provider output, retaining tabs and newlines.
  return String(value).replace(/\x1b(?:\][^\x07]*(?:\x07|\x1b\\)|\[[0-?]*[ -/]*[@-~])/g, '').replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, '');
}
async function main() {
  const { values: v, positionals } = parseArgs({ allowPositionals: true, options: {
    help: { type: 'boolean', short: 'h' }, prompt: { type: 'string', short: 'p' },
    version: { type: 'boolean', short: 'v' },
    resume: { type: 'string', short: 'r' }, continue: { type: 'boolean', short: 'c' },
    cwd: { type: 'string' }, order: { type: 'string' }, write: { type: 'boolean' },
    provider: { type: 'string' }, model: { type: 'string', multiple: true }, config: { type: 'string' },
    timeout: { type: 'string' }, retry: { type: 'boolean' }, json: { type: 'boolean' },
  } });
  if (v.help) { print(help); return; }
  if (v.version || positionals[0] === 'version') { print('0.1.0'); return; }
  const env = { ...process.env, ...(v.config ? { LENNON_CONFIG: v.config } : {}) };
  const root = stateRoot(env);
  if (positionals[0] === 'config' && positionals.length > 1 && positionals[1] !== 'show') {
    const { editConfig } = await import('../src/config-command.js');
    print(editConfig(positionals.slice(1), env)); return;
  }
  const config = applyOverrides(loadConfig(env, Boolean(v.config)), { order: v.order, provider: v.provider, models: v.model });
  if (positionals[0] === 'config') {
    if (positionals.length > 2) throw new Error('Usage: lennon config [show]');
    print(JSON.stringify({ path: configPath(env), state: root, ...config }, null, 2)); return;
  }
  if (positionals[0] === 'providers') {
    for (const [id, p] of Object.entries(config.providers)) {
      const order = config.order.indexOf(id);
      if (v.json) print(JSON.stringify({ id, ...p, order: order < 0 ? null : order + 1 }));
      else print(`${id}  ${p.adapter}  ${p.model || '(native default)'}  ${p.enabled ? order < 0 ? 'not in order' : `#${order + 1}` : 'disabled'}`);
    }
    return;
  }
  if (positionals[0] === 'doctor') {
    let available = 0;
    for (const name of config.order) {
      if (!config.providers[name].enabled) continue;
      const command = config.providers[name]?.command || adapterFor(name, config);
      const result = spawnSync(command, ['--version'], { encoding: 'utf8', timeout: 10000, shell: false });
      const ok = !result.error && result.status === 0;
      if (ok) available++;
      print(`${name.padEnd(7)} ${ok ? safeText(result.stdout || result.stderr).trim() : `unavailable: ${result.error?.message || result.stderr}`}`);
    }
    print('Login using: claude / codex login / gemini. Doctor checks executables, not account quotas.');
    if (!available) process.exitCode = 1;
    return;
  }
  if (positionals[0] === 'sessions') {
    const sessions = listSessions(root);
    if (v.json) for (const s of sessions) print(JSON.stringify(s));
    else for (const s of sessions) print(`${s.id}  ${s.updated}  ${s.pending ? 'unfinished' : 'complete'}  ${safeText(s.cwd)}`);
    return;
  }
  const timeoutMs = v.timeout === undefined ? 0 : Number(v.timeout);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 0) throw new Error('--timeout must be a non-negative integer');
  if (v.resume && v.continue) throw new Error('Choose --resume or --continue');
  const cwd = realpathSync(v.cwd || process.cwd());
  let id = v.resume;
  if (v.continue) {
    id = listSessions(root).find(s => s.cwd === cwd)?.id;
    if (!id) throw new Error('No previous Lennon session in this directory');
  }
  let prompt = v.prompt ?? (positionals.length ? positionals.join(' ') : undefined);
  const oneShot = prompt !== undefined || !stdin.isTTY || Boolean(id);
  if (!stdin.isTTY) {
    let piped = '';
    for await (const chunk of stdin) {
      piped += chunk;
      if (piped.length > 4 * 1024 * 1024) throw new Error('Piped prompt exceeds 4 MiB');
    }
    if (piped.trim()) prompt = prompt ? `${prompt}\n\n${piped}` : piped;
  }
  const session = id ? new Session(root, id) : Session.create(root, cwd);
  if (id && v.cwd && session.meta.cwd !== cwd) throw new Error('Session belongs to a different working directory');
  session.lock();
  let rl;
  const controller = new AbortController();
  const stop = () => controller.abort();
  process.on('SIGINT', stop); process.on('SIGTERM', stop);
  const emit = event => print(JSON.stringify({ session: session.meta.id, ...event }));
  const status = text => v.json ? emit({ type: 'status', text }) : stderr.write(`\n[lennon] ${safeText(text)}\n`);
  try {
    if (v.retry) { session.meta.cooldowns = {}; session.save(); }
    status(`session ${session.meta.id} · ${session.meta.cwd}`);
    const turn = async text => {
      const result = await runTurn(session, text, config, {
        signal: controller.signal, timeoutMs, permission: v.write ? 'write' : 'read-only',
        onStatus: status, onText: text => v.json ? emit({ type: 'text', text }) : stdout.write(safeText(text)),
      });
      if (v.json) emit({ type: 'result', ...result });
      else stdout.write('\n');
      if (result.status !== 'success') {
        if (result.error) status(typeof result.error === 'string' ? result.error : JSON.stringify(result.error));
        status(`Saved. Continue with: lennon --resume ${session.meta.id}${v.write ? ' --write' : ''}`);
        process.exitCode = result.status === 'cancelled' ? 130 : 1;
      }
      return result;
    };
    if (oneShot) { await turn(prompt); return; }
    if (v.json) throw new Error('Interactive mode requires text output; supply --prompt with --json');
    stderr.write(`lennon · /help for commands · ${config.order.filter(id => config.providers[id].enabled).join(' → ')}\n`);
    rl = createInterface({ input: stdin, output: stdout });
    rl.on('SIGINT', stop);
    while (!controller.signal.aborted) {
      let text;
      try { text = await rl.question('\nlennon › ', { signal: controller.signal }); }
      catch (e) { if (e.name === 'AbortError' || e.code === 'ERR_USE_AFTER_CLOSE') break; throw e; }
      text = text.trim();
      if (!text) continue;
      if (text === '/exit' || text === '/quit') break;
      if (text === '/help') { print('/status · /retry · /exit\nType a task to continue the same session.'); continue; }
      if (text === '/status') { print(JSON.stringify(session.meta, null, 2)); continue; }
      if (text === '/retry') { session.meta.cooldowns = {}; session.save(); if (session.meta.pending) await turn(); continue; }
      if (text.startsWith('/')) { print('Unknown command. Use /help.'); continue; }
      const result = await turn(text);
      if (result.status === 'cancelled') break;
      process.exitCode = 0;
    }
  } finally {
    rl?.close(); session.unlock();
    process.removeListener('SIGINT', stop); process.removeListener('SIGTERM', stop);
  }
}
main().catch(error => { stderr.write(`lennon: ${safeText(error.message)}\n`); process.exitCode = 1; });
