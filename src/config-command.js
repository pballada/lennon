import { mkdirSync, openSync, writeSync, fsyncSync, closeSync } from 'node:fs';
import { dirname } from 'node:path';
import { configPath, readConfig, normalizeConfig } from './config.js';
import { atomicJSON } from './store.js';

export function editConfig(args, env = process.env) {
  const [action, key, value, ...rest] = args;
  const path = configPath(env);
  if (action === 'init') {
    if (key !== undefined) throw new Error('Usage: lennon config init');
    const template = { order: ['claude', 'codex', 'gemini'], providers: {
      claude: { adapter: 'claude', enabled: true }, codex: { adapter: 'codex', enabled: true }, gemini: { adapter: 'gemini', enabled: true },
    } };
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    const fd = openSync(path, 'wx', 0o600);
    try { writeSync(fd, JSON.stringify(template, null, 2) + '\n'); fsyncSync(fd); }
    finally { closeSync(fd); }
    return path;
  }
  if (!['set', 'unset'].includes(action) || !key || rest.length || (action === 'set' ? value === undefined : value !== undefined)) {
    throw new Error('Usage: lennon config init | set KEY VALUE | unset KEY');
  }
  const parts = key.split('.');
  if (parts.some(part => !/^[a-zA-Z][a-zA-Z0-9_-]*$/.test(part) || ['constructor','prototype','__proto__'].includes(part)) || parts.length > 3) throw new Error('Invalid config key');
  const user = readConfig(env);
  normalizeConfig(user);
  let parsed = value;
  if (action === 'set') {
    if (key === 'order') parsed = value.split(',').map(id => id.trim());
    else if (['cooldownMs','handoffChars'].includes(key)) parsed = Number(value);
    else if (parts.at(-1) === 'enabled' || (parts[0] === 'providers' && parts.length <= 2)) {
      try { parsed = JSON.parse(value); } catch { throw new Error(`Expected JSON for ${key}`); }
    }
  }
  let target = user;
  for (const part of parts.slice(0, -1)) {
    if (!Object.hasOwn(target, part)) target[part] = {};
    if (!target[part] || typeof target[part] !== 'object' || Array.isArray(target[part])) throw new Error(`Invalid config key: ${key}`);
    target = target[part];
  }
  if (action === 'unset') delete target[parts.at(-1)];
  else target[parts.at(-1)] = parsed;
  normalizeConfig(user);
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  atomicJSON(path, user);
  return path;
}
