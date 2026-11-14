import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export const names = ['claude', 'codex', 'gemini'];
export function stateRoot(env = process.env) {
  return env.LENNON_STATE_DIR || join(env.XDG_STATE_HOME || join(homedir(), '.local', 'state'), 'lennon');
}
export function configPath(env = process.env) {
  return env.LENNON_CONFIG || join(env.XDG_CONFIG_HOME || join(homedir(), '.config'), 'lennon', 'config.json');
}
export function loadConfig(env = process.env) {
  let user = {};
  try { user = JSON.parse(readFileSync(configPath(env), 'utf8')); }
  catch (e) { if (e.code !== 'ENOENT') throw new Error(`Invalid config: ${e.message}`); }
  const config = {
    order: names, cooldownMs: 300_000, handoffChars: 120_000,
    providers: {}, ...user,
  };
  if (!config.providers || typeof config.providers !== 'object' || Array.isArray(config.providers)) throw new Error('providers must be an object');
  if (config.authentication && !['subscription', 'environment'].includes(config.authentication)) throw new Error('authentication must be subscription or environment');
  if (!Array.isArray(config.order) || !config.order.length ||
      config.order.some(n => !names.includes(n)) || new Set(config.order).size !== config.order.length) {
    throw new Error('order must contain unique names from claude, codex, gemini');
  }
  for (const key of ['cooldownMs', 'handoffChars']) {
    if (!Number.isSafeInteger(config[key]) || config[key] < (key === 'handoffChars' ? 1000 : 0)) {
      throw new Error(`Invalid ${key}`);
    }
  }
  for (const name of names) {
    const p = config.providers[name] || {};
    if (p.command !== undefined && (typeof p.command !== 'string' || !p.command)) throw new Error(`Invalid ${name} command`);
    if (p.model !== undefined && (typeof p.model !== 'string' || !p.model)) throw new Error(`Invalid ${name} model`);
    if (p.allowedTools !== undefined && typeof p.allowedTools !== 'string') throw new Error(`Invalid ${name} allowedTools`);
  }
  return config;
}
