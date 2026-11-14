# Validation · native Rust 0.2.0 · 2026-11-14

## Automated checks

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test --all-features`: **32 passed** (10 unit + 22 native integration).
- `cargo build --release --bin lennon`: passed; approximately 1.7 MiB executable.
- Cargo package inventory excludes archived JavaScript and benchmark sources.

Native tests launch compiled mock providers without Node, Python, credentials,
model spend, or network access. They cover quota failover, cooldown exhaustion,
auth/permission errors, independent model entries, config editing/validation,
model changes, context replay, native resume, UTF-8 streaming, missing and malformed
provider output, event-size limits, secret-environment handling, timeouts, Ctrl-C,
process groups, uncooperative children, journal recovery, private permissions,
exclusive locks, stale-lock recovery, and bounded handoff archives.

## Existing session compatibility

The comparative experiment creates a real session with the archived JavaScript
CLI, resumes it using the Rust CLI, and resumes it again using the JavaScript
CLI. **JS → Rust → JS passed**, retaining the same native provider session.
Version-1 JSON/JSONL state remains compatible; no session migration is required.

## Live provider checks

- Claude Code 2.1.325: Rust CLI returned the requested token in a read-only run;
  a second run resumed the native session and recalled the same token.
- Codex CLI 0.162.0: Rust CLI returned the requested token in a read-only run;
  a second run resumed the native thread and recalled the same token.
- Gemini: native mock-protocol and resume tests pass. Gemini CLI is not installed
  here, so live authentication and execution remain unverified.
- Real quota exhaustion was not deliberately induced; quota routing uses native
  fixture processes in automated tests.

## Full implementation performance

On Apple M4/macOS, using release Rust and Node 26.10.0:

| Median | Rust | Previous JavaScript CLI |
| --- | ---: | ---: |
| Fresh-process full CLI startup | 3.82 ms | 48.71 ms |
| Complete controlled mock-provider failover | 113.97 ms | 166.81 ms |
| Peak RSS, full CLI help | 6.00 MiB | 48.17 MiB |

Five warmups, randomized balanced blocks, 100 startup samples, 25 failover samples,
and five separate memory samples. The same native fixtures serve both versions.
Full-sync-per-journal-event durability was retained. These measurements exclude
live provider/model network latency and do not predict long-session memory.

[Method and raw data](benchmarks/NATIVE_PORT.md). The earlier language-probe study
is retained as historical context, not substituted for the full native benchmark.

CI now verifies the Rust application on macOS and Linux, plus Rust 1.85 minimum
compatibility on Linux. JavaScript is retained only as a benchmark/compatibility
reference and is not shipped in the crate or invoked by the native command.
