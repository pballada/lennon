import { buildPrompt } from './context.js';
import { invocation } from './providers.js';
import { runProvider } from './runner.js';
import { normalizeConfig, adapterFor, routeIdentity } from './config.js';

export async function runTurn(session, text, config, options = {}) {
  config = normalizeConfig(config);
  const { signal, onText = () => {}, onStatus = () => {}, permission = 'read-only', timeoutMs = 0, run = runProvider } = options;
  if (text) {
    session.append({ type: 'user', text });
    session.meta.pending = true; session.save();
  }
  if (!session.meta.pending) throw new Error('No unfinished task. Supply a prompt.');
  const reasons = [];
  for (const name of config.order) {
    if (!config.providers[name].enabled) continue;
    if (signal?.aborted) return { status: 'cancelled' };
    const adapter = adapterFor(name, config);
    const identity = routeIdentity(name, config);
    const native = session.meta.providers[name];
    // Models and executables can have incompatible native histories. Transfer the
    // complete portable transcript into a fresh native session if a route changes.
    if (native && JSON.stringify(native.route || { adapter: name, command: name, model: null }) !== JSON.stringify(identity)) {
      session.append({ type: 'route_reset', provider: name, route: identity });
      delete session.meta.providers[name]; delete session.meta.cooldowns[name]; session.save();
    }
    const until = session.meta.cooldowns[name] || 0;
    if (until > Date.now()) {
      reasons.push(`${name}: cooling down until ${new Date(until).toISOString()}`);
      continue;
    }
    const current = session.meta.providers[name];
    const prompt = buildPrompt(session, name, config);
    const spec = invocation(name, current?.id, config, permission);
    session.append({ type: 'attempt_start', provider: name, adapter, model: identity.model, route: identity, nativeId: current?.id, permission });
    session.meta.providers[name] = { ...current, route: identity };
    onStatus(`${name}${identity.model ? ` · ${identity.model}` : ''}${current?.id ? ' · resuming' : ''}`);
    const result = await run({
      name: adapter, ...spec, cwd: session.meta.cwd, prompt, signal, timeoutMs,
      authentication: config.authentication || 'subscription',
      onEvent: event => {
        session.append({ type: 'provider_event', provider: name, adapter, event });
        const id = adapter === 'codex' && event.type === 'thread.started' ? event.thread_id :
          (event.type === 'init' || (event.type === 'system' && event.subtype === 'init')) ? event.session_id : undefined;
        if (typeof id === 'string' && id && session.meta.providers[name]?.id !== id) {
          session.append({ type: 'native_session', provider: name, id, route: identity });
          session.meta.providers[name] = { ...session.meta.providers[name], id, route: identity }; session.save();
        }
      }, onText, onActivity: activity => onStatus(`${name} · ${activity}`),
    });
    session.append({ type: 'attempt_end', provider: name, status: result.status, error: result.error });
    if (result.status === 'success') {
      session.append({ type: 'complete', provider: name });
      session.meta.providers[name] = { ...session.meta.providers[name], cursor: session.events().length };
      session.meta.pending = false; delete session.meta.cooldowns[name]; session.save();
      return { status: 'success', provider: name };
    }
    if (result.status === 'cancelled') { session.save(); return result; }
    if (['limit', 'transient', 'missing'].includes(result.status)) {
      const reason = typeof result.error === 'string' ? result.error : JSON.stringify(result.error);
      reasons.push(`${name}: ${result.status} — ${reason}`);
      if (result.status !== 'missing') session.meta.cooldowns[name] = Date.now() + config.cooldownMs;
      session.save(); onStatus(`${name}: ${result.status}; trying next provider`);
      continue;
    }
    session.save();
    return { ...result, provider: name };
  }
  session.save();
  return { status: 'exhausted', error: reasons.join('\n') };
}
