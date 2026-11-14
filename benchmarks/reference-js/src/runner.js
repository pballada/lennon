import { spawn } from 'node:child_process';
import { StringDecoder } from 'node:string_decoder';
import { decode, classify } from './providers.js';

export function runProvider({ name, command, args, cwd, prompt, signal, timeoutMs = 0, onEvent, onText, onActivity = () => {}, environment = process.env, authentication = 'subscription' }) {
  return new Promise(resolve => {
    const env = { ...environment };
    if (authentication === 'subscription') {
      for (const key of ['ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'OPENAI_API_KEY', 'GEMINI_API_KEY', 'GOOGLE_API_KEY']) delete env[key];
    }
    let child;
    try { child = spawn(command, args, { cwd, env, stdio: ['pipe', 'pipe', 'pipe'], shell: false, detached: process.platform !== 'win32' }); }
    catch (error) { resolve({ status: error.code === 'ENOENT' ? 'missing' : 'fatal', error: error.message }); return; }
    const decoder = new StringDecoder('utf8');
    let buffer = '', stderr = '', output = '', error, nativeId, complete = false;
    let aborted = false, timedOut = false, forced, killTimer, timer, callbackError;
    const kill = () => {
      if (!child.pid) return;
      const send = sig => {
        try { process.kill(process.platform === 'win32' ? child.pid : -child.pid, sig); }
        catch (e) { if (e.code !== 'ESRCH') child.kill(sig); }
      };
      send('SIGTERM');
      killTimer ||= setTimeout(() => send('SIGKILL'), 1500);
      killTimer.unref();
    };
    const abort = () => { aborted = true; kill(); };
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    if (timeoutMs > 0) timer = setTimeout(() => { timedOut = true; kill(); }, timeoutMs);
    const line = value => {
      if (!value.trim() || callbackError) return;
      let event;
      try { event = JSON.parse(value); }
      catch { stderr = (stderr + '\n' + value).slice(-32000); return; }
      try {
        onEvent(event);
        const d = decode(name, event);
        if (d.activity) onActivity(d.activity);
        if (d.nativeId) nativeId = d.nativeId;
        if (d.error) {
          error = d.error;
          // Stop upstream retry loops promptly once an explicit quota error arrives.
          if (classify(d.error) === 'limit') { forced = 'limit'; kill(); }
        }
        if (d.text) { output += d.text; onText(d.text); }
        if (d.complete) {
          complete = true;
          if (!output && d.finalText) { output = d.finalText; onText(d.finalText); }
        }
      } catch (e) { callbackError = e; kill(); }
    };
    child.stdout.on('data', chunk => {
      buffer += decoder.write(chunk);
      let end;
      while ((end = buffer.indexOf('\n')) !== -1) {
        line(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
      }
      if (buffer.length > 16 * 1024 * 1024) { callbackError = new Error('Provider JSON event exceeds 16 MiB'); kill(); }
    });
    child.stderr.on('data', chunk => { stderr = (stderr + chunk.toString()).slice(-32000); });
    child.stdin.on('error', e => { if (e.code !== 'EPIPE') { error = { message: e.message }; kill(); } });
    child.on('error', e => { error = { code: e.code, message: e.message }; });
    child.on('close', code => {
      clearTimeout(timer); clearTimeout(killTimer);
      signal?.removeEventListener('abort', abort);
      buffer += decoder.end();
      if (buffer.trim()) line(buffer);
      const failure = callbackError?.message || error || stderr || `Provider exited with code ${code} without a successful result event`;
      const status = aborted ? 'cancelled' : callbackError ? 'fatal' : forced || (timedOut ? 'timeout' :
        error?.code === 'ENOENT' ? 'missing' : code === 0 && complete ? 'success' : classify(failure));
      resolve({ status, error: status === 'success' ? undefined : failure, nativeId, code });
    });
    child.stdin.end(prompt);
  });
}
