import { adapterFor } from './config.js';

export function invocation(id, nativeId, config, permission = 'read-only') {
  const name = adapterFor(id, config);
  const p = config.providers[id] || {};
  let args;
  if (name === 'claude') {
    args = ['-p', '--verbose', '--output-format', 'stream-json', '--include-partial-messages'];
    if (nativeId) args.push('--resume', nativeId);
    if (permission === 'read-only') args.push('--permission-mode', 'plan', '--allowedTools', 'Read,Glob,Grep', '--disallowedTools', 'Bash,Write,Edit,NotebookEdit');
    else args.push('--permission-mode', 'acceptEdits');
    if (permission === 'write' && p.allowedTools) args.push('--allowedTools', p.allowedTools);
  } else if (name === 'codex') {
    args = ['exec'];
    // Global config works for both exec and exec resume; resume has no --sandbox flag.
    args.push('-c', `sandbox_mode="${permission === 'read-only' ? 'read-only' : 'workspace-write'}"`);
    args.push('-c', 'approval_policy="never"');
    if (nativeId) args.push('resume', nativeId);
    args.push('--json');
  } else if (name === 'gemini') {
    args = ['--output-format', 'stream-json', '--approval-mode', permission === 'read-only' ? 'plan' : 'auto_edit'];
    if (nativeId) args.push('--resume', nativeId);
    // The full prompt travels on stdin, avoiding command length limits and ps exposure.
    args.push('-p', 'Continue the task supplied on standard input.');
  } else throw new Error(`Unknown provider: ${name}`);
  if (p.model) args.push('--model', p.model);
  if (name === 'codex') args.push('-');
  return { command: p.command || name, args };
}

export function decode(name, e) {
  const out = { text: '', nativeId: undefined, error: undefined, complete: false };
  if (name === 'claude') {
    if (e.type === 'system' && e.subtype === 'init') out.nativeId = e.session_id;
    if (e.type === 'stream_event' && e.event?.type === 'content_block_delta' && e.event.delta?.type === 'text_delta') out.text = e.event.delta.text;
    if (e.type === 'stream_event' && e.event?.type === 'content_block_start' && e.event.content_block?.type === 'tool_use') out.activity = e.event.content_block.name;
    if (e.type === 'assistant' && e.error) out.error = { code: e.error, message: JSON.stringify(e.message?.content || '') };
    if (e.type === 'result') {
      out.complete = e.subtype === 'success' && !e.is_error;
      if (!out.complete) out.error = { code: e.subtype, message: e.errors?.join('\n') || e.result || 'Claude run failed' };
      out.finalText = e.result;
    }
  } else if (name === 'codex') {
    if (e.type === 'thread.started') out.nativeId = e.thread_id;
    if (e.type === 'item.completed' && e.item?.type === 'agent_message') out.text = e.item.text || '';
    if (e.type === 'item.started' && e.item?.type !== 'reasoning') out.activity = e.item?.type;
    if (e.type === 'turn.completed') out.complete = true;
    if (e.type === 'turn.failed' || e.type === 'error') out.error = e.error || { message: e.message || 'Codex run failed' };
  } else {
    if (e.type === 'init') out.nativeId = e.session_id;
    if (e.type === 'message' && e.role === 'assistant') out.text = e.content || '';
    if (e.type === 'tool_use') out.activity = e.tool_name;
    if (e.type === 'result') {
      out.complete = e.status === 'success';
      if (!out.complete) out.error = e.error || { message: e.message || 'Gemini run failed' };
    }
    if (e.type === 'error' && e.severity !== 'warning') out.error = e.error || { code: e.code, message: e.message };
  }
  return out;
}

export function classify(error) {
  const text = typeof error === 'string' ? error : JSON.stringify(error || {});
  if (/rate[_ -]?limit|usage[_ -]?limit|quota[_ -]?(?:exceeded|exhausted)|resource_exhausted|insufficient_quota|too many requests|hit your limit|out of extra usage|\b429\b|credit balance is too low|(?:exceeded|exhausted).*(?:quota|usage|capacity)/i.test(text)) return 'limit';
  if (/overloaded|\b(?:500|502|503|504|529)\b|service unavailable|ECONNRESET|ETIMEDOUT|ENETUNREACH|EAI_AGAIN|fetch failed/i.test(text)) return 'transient';
  return 'fatal';
}

// Only observable messages/tool activity are portable. Private model state is not.
export function portable(name, e) {
  if (name === 'claude') {
    if (e.type === 'assistant' || e.type === 'user') {
      const content = e.message?.content;
      if (typeof content === 'string') return { role: e.type, content };
      const blocks = content?.filter(b => ['text', 'tool_use', 'tool_result'].includes(b.type));
      if (blocks?.length) return { role: e.type, content: blocks };
    }
    if (e.type === 'stream_event' && e.event?.delta?.type === 'text_delta') return { role: 'assistant_delta', content: e.event.delta.text };
  } else if (name === 'codex') {
    if (e.type === 'item.completed' && e.item?.type !== 'reasoning') return { role: 'assistant_activity', content: e.item };
    if (e.type === 'item.started') return { role: 'tool_started', content: e.item };
  } else if (['message', 'tool_use', 'tool_result'].includes(e.type) && e.role !== 'user') {
    return { role: e.type, content: e };
  }
  return undefined;
}
