import { mkdirSync, openSync, closeSync, writeSync, fsyncSync, readFileSync, renameSync, unlinkSync, readdirSync, realpathSync, truncateSync } from 'node:fs';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

const idPattern = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;
export function atomicJSON(path, value) {
  const temp = `${path}.${process.pid}.${randomUUID()}.tmp`;
  const fd = openSync(temp, 'wx', 0o600);
  try { writeSync(fd, JSON.stringify(value, null, 2) + '\n'); fsyncSync(fd); }
  finally { closeSync(fd); }
  renameSync(temp, path);
}
export class Session {
  constructor(root, id) {
    if (!idPattern.test(id)) throw new Error('Invalid session ID');
    this.dir = join(root, 'sessions', id);
    this.path = join(this.dir, 'session.json');
    this.journal = join(this.dir, 'events.jsonl');
    this.meta = JSON.parse(readFileSync(this.path, 'utf8'));
  }
  static create(root, cwd) {
    const id = randomUUID();
    const dir = join(root, 'sessions', id);
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    atomicJSON(join(dir, 'session.json'), {
      version: 1, id, cwd: realpathSync(cwd), created: new Date().toISOString(),
      updated: new Date().toISOString(), providers: {}, cooldowns: {}, pending: false,
    });
    return new Session(root, id);
  }
  lock() {
    const path = join(this.dir, 'lock');
    try {
      this.lockFd = openSync(path, 'wx', 0o600);
    } catch (e) {
      if (e.code !== 'EEXIST') throw e;
      const pid = Number(readFileSync(path, 'utf8'));
      if (!Number.isInteger(pid) || pid <= 0) throw new Error(`Session lock is invalid: ${path}`);
      try { process.kill(pid, 0); }
      catch (error) {
        if (error.code !== 'ESRCH') throw error;
        // Unlink the stale lock, then compete with other writers using O_EXCL.
        try { unlinkSync(path); } catch (error) { if (error.code !== 'ENOENT') throw error; }
        return this.lock();
      }
      throw new Error(`Session is already running (PID ${pid})`);
    }
    writeSync(this.lockFd, String(process.pid));
    fsyncSync(this.lockFd);
    try {
    // Reload after acquiring ownership: a previous writer may have just finished.
    this.meta = JSON.parse(readFileSync(this.path, 'utf8'));
    // Repair only an incomplete final record left by a killed writer.
    try {
      const data = readFileSync(this.journal);
      if (data.length && data.at(-1) !== 10) truncateSync(this.journal, data.lastIndexOf(10) + 1);
    } catch (e) { if (e.code !== 'ENOENT') throw e; }
    // The append-only journal is authoritative if metadata saving was interrupted.
    const records = this.events();
    const lastUser = records.findLastIndex(e => e.type === 'user');
    const lastDone = records.findLastIndex(e => e.type === 'complete');
    this.meta.pending = lastUser > lastDone;
    for (let i = 0; i < records.length; i++) {
      const e = records[i];
      if (e.type === 'native_session') this.meta.providers[e.provider] = { ...this.meta.providers[e.provider], id: e.id };
      if (e.type === 'complete') this.meta.providers[e.provider] = { ...this.meta.providers[e.provider], cursor: i + 1 };
    }
    } catch (e) { this.unlock(); throw e; }
  }
  unlock() {
    if (this.lockFd === undefined) return;
    closeSync(this.lockFd); this.lockFd = undefined;
    unlinkSync(join(this.dir, 'lock'));
  }
  save() { this.meta.updated = new Date().toISOString(); atomicJSON(this.path, this.meta); }
  append(event) {
    const fd = openSync(this.journal, 'a', 0o600);
    try { writeSync(fd, JSON.stringify({ time: new Date().toISOString(), ...event }) + '\n'); fsyncSync(fd); }
    finally { closeSync(fd); }
  }
  events() {
    let raw;
    try { raw = readFileSync(this.journal, 'utf8'); } catch (e) { if (e.code === 'ENOENT') return []; throw e; }
    return raw.split('\n').filter(Boolean).map(line => JSON.parse(line));
  }
}
export function listSessions(root) {
  let entries;
  try { entries = readdirSync(join(root, 'sessions')); } catch (e) { if (e.code === 'ENOENT') return []; throw e; }
  return entries.filter(id => idPattern.test(id)).map(id => new Session(root, id).meta)
    .sort((a, b) => b.updated.localeCompare(a.updated));
}
