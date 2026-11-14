import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export const names = ['claude', 'codex', 'gemini'];
const own = (object, key) => Object.hasOwn(object, key);
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const validId = id => /^[a-z][a-z0-9_-]{0,63}$/.test(id) && !['constructor', 'prototype', '__proto__'].includes(id);
const text = value => typeof value === 'string' && value.trim().length > 0 && !value.includes('\0');
export function stateRoot(env = process.env) {
  return env.LENNON_STATE_DIR || join(env.XDG_STATE_HOME || join(homedir(), '.local', 'state'), 'lennon');
}
export function configPath(env = process.env) {
  return env.LENNON_CONFIG || join(env.XDG_CONFIG_HOME || join(homedir(), '.config'), 'lennon', 'config.json');
}
export function readConfig(env = process.env, required = false) {
  try { return JSON.parse(readFileSync(configPath(env), 'utf8')); }
  catch (e) {
    if (e.code === 'ENOENT' && !required) return {};
    throw new Error(`Invalid config ${configPath(env)}: ${e.message}`);
  }
}
export function normalizeConfig(user = {}) {
  if (!object(user)) throw new Error('config must be an object');
  for (const key of Object.keys(user)) {
    if (!['order', 'cooldownMs', 'handoffChars', 'authentication', 'providers'].includes(key)) throw new Error(`Unknown config setting: ${key}`);
  }
  const config = { order: [...names], cooldownMs: 300_000, handoffChars: 120_000, authentication: 'subscription', providers: {}, ...user };
  if (!object(config.providers)) throw new Error('providers must be an object');
  if (!['subscription', 'environment'].includes(config.authentication)) throw new Error('authentication must be subscription or environment');
  const providers = Object.fromEntries(names.map(name => [name, { adapter: name, enabled: true }]));
  for (const [id, p] of Object.entries(config.providers)) {
    if (!validId(id)) throw new Error(`Invalid provider ID: ${id}`);
    if (!object(p)) throw new Error(`providers.${id} must be an object`);
    for (const key of Object.keys(p)) {
      if (!['adapter', 'command', 'model', 'enabled', 'allowedTools'].includes(key)) throw new Error(`Unknown providers.${id} setting: ${key}`);
    }
    const entry = { enabled: true, ...providers[id], ...p };
    if (!names.includes(entry.adapter)) throw new Error(`providers.${id}.adapter must be claude, codex, or gemini`);
    if (typeof entry.enabled !== 'boolean') throw new Error(`providers.${id}.enabled must be boolean`);
    for (const key of ['command', 'model']) {
      if (own(entry, key) && (!text(entry[key]) || (key === 'model' && entry[key].startsWith('-')))) throw new Error(`Invalid providers.${id}.${key}`);
    }
    if (own(entry, 'allowedTools') && typeof entry.allowedTools !== 'string') throw new Error(`Invalid providers.${id}.allowedTools`);
    providers[id] = entry;
  }
  config.providers = providers;
  if (!Array.isArray(config.order) || !config.order.length || new Set(config.order).size !== config.order.length ||
      config.order.some(id => typeof id !== 'string' || !own(providers, id))) {
    throw new Error('order must contain unique configured provider IDs');
  }
  if (!config.order.some(id => providers[id].enabled)) throw new Error('order must include at least one enabled provider');
  config.order = [...config.order];
  for (const key of ['cooldownMs', 'handoffChars']) {
    if (!Number.isSafeInteger(config[key]) || config[key] < (key === 'handoffChars' ? 1000 : 0)) throw new Error(`Invalid ${key}`);
  }
  return config;
}
export function loadConfig(env = process.env, required = false) {
  return normalizeConfig(readConfig(env, required));
}
export function applyOverrides(config, { order, provider, models = [] } = {}) {
  if (order !== undefined && provider !== undefined) throw new Error('Choose --order or --provider');
  const providers = Object.fromEntries(Object.entries(config.providers).map(([id, p]) => [id, { ...p }]));
  const seen = new Set();
  for (const assignment of models) {
    const split = assignment.indexOf('=');
    const id = assignment.slice(0, split), model = assignment.slice(split + 1);
    if (split < 1 || !own(providers, id) || !model || seen.has(id)) throw new Error('--model requires one PROVIDER=MODEL assignment per configured provider');
    seen.add(id); providers[id].model = model;
  }
  return normalizeConfig({ ...config, providers, order: provider !== undefined ? [provider] : order !== undefined ? order.split(',').map(id => id.trim()) : config.order });
}
export function adapterFor(id, config) { return config.providers[id]?.adapter || (names.includes(id) ? id : undefined); }
export function routeIdentity(id, config) {
  const p = config.providers[id] || {};
  return { adapter: adapterFor(id, config), command: p.command || adapterFor(id, config), model: p.model ?? null };
}
