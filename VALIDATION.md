# Validation · 2026-11-14

Local verification on macOS:

- `npm run check`: 18 tests passed; CLI syntax checked.
- `npm pack --dry-run`: package contains only the intended distributable files.
- `npm run bench`: 20 fresh processes, median 47.2 ms, p95 51.6 ms for `--help`.
  This is harness overhead, not a latency promise for any model or upstream CLI.
- Claude Code **2.1.325**: live subscription-backed read-only request succeeded.
  A second turn resumed the recorded native session and recalled the prior reply.
- Codex CLI **0.162.0**: live subscription-backed read-only request succeeded.
  A second turn resumed the recorded native thread and recalled the prior reply.
- Gemini CLI: not installed on the development machine. Adapter flags and event
  shapes follow the official headless/session documentation and are fixture-tested;
  live authentication, execution, and resume remain unverified.

Real quota exhaustion was not deliberately induced. The automated suite launches
mock provider processes to verify streamed quota handling and end-to-end failover.
No claims of complete upstream feature parity, identical hidden context, or
exactly-once side effects are made.

CI runs the automated suite on Linux and macOS with Node 22 and 24.
