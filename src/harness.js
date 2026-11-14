import { buildPrompt } from './context.js';
import { invocation } from './providers.js';
import { runProvider } from './runner.js';

export async function runTurn(session, text, config, options = {}) {
  const { signal, onText = () => {}, onStatus = () => {}, permission = 'read-only', timeoutMs = 0, run = runProvider } = options;
  if (text) {
    session.append({ type: 'user', text });
    session.meta.pending = true; session.save();
  }
  if (!session.meta.pending) throw new Error('No unfinished task. Supply a prompt.');
  const reasons = [];
  for (const name of config.order) {
    if (signal?.aborted) return { status: 'cancelled' };
    const until = session.meta.cooldowns[name] || 0;
    if (until > Date.now()) {
      reasons.push(`${name}: cooling down until ${new Date(until).toISOString()}`);
      continue;
    }
    const native = session.meta.providers[name];
    const prompt = buildPrompt(session, name, config);
    const spec = invocation(name, native?.id, config, permission);
    session.append({ type: 'attempt_start', provider: name, nativeId: native?.id, permission });
    onStatus(`${name}${native?.id ? ' · resuming' : ''}`);
    const result = await run({
      name, ...spec, cwd: session.meta.cwd, prompt, signal, timeoutMs,
      authentication: config.authentication || 'subscription',
      onEvent: event => {
        session.append({ type: 'provider_event', provider: name, event });
        const id = name === 'codex' && event.type === 'thread.started' ? event.thread_id :
          (event.type === 'init' || (event.type === 'system' && event.subtype === 'init')) ? event.session_id : undefined;
        if (typeof id === 'string' && id && session.meta.providers[name]?.id !== id) {
          session.append({ type: 'native_session', provider: name, id });
          session.meta.providers[name] = { ...session.meta.providers[name], id }; session.save();
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
