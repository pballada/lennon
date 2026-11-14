# Native Rust port · 2026-11-14

This measures the complete Rust 0.2.0 CLI against the archived JavaScript 0.1.0
implementation on this Apple M4 Mac. No live model generation is included.

| Metric (median) | Rust | JavaScript / Node |
| --- | ---: | ---: |
| Fresh-process full CLI startup | 3.82 ms | 48.71 ms |
| Complete mock-provider failover | 113.97 ms | 166.81 ms |
| Peak RSS, full CLI help | 6.00 MiB | 48.17 MiB |

Startup improved 12.8×. The complete failover was 31.7% faster in this controlled fixture.

The release executable is approximately 1.7 MiB and runs without Node.js or a
JavaScript fallback. Ordinary Rust/system libraries are linked as part of the
native executable; upstream provider CLIs retain their own runtime requirements.

## Method

- Five warmups per variant; 100 startup samples and 25 complete failover samples.
- Balanced randomized order, warm filesystem caches, fresh process each sample.
- Same native mock provider executable for both implementations.
- Primary emits tool activity, a Unicode partial reply, then a quota error.
- Fallback validates the task and recorded tool results and completes.
- Both implementations sync each journal event; Rust also syncs its handoff file.
- Same legacy JSON config and version-1 journal/session formats.
- Memory measured separately with macOS time(1), five samples per variant.
- No outlier removal. Raw p95, extrema, samples, environment, and binary SHA-256
  are included in the artifact.

## Compatibility

The comparison explicitly creates a session in the JavaScript CLI, resumes it
in Rust, then resumes it back in JavaScript. Both transitions retain the same
native provider session ID. This check passed.

## Reproduce

```sh
cargo build --locked --release --bin lennon
cargo build --locked --features test-fixtures --bin lennon-provider-fixture
python3 benchmarks/native-port.py
```

Python and Node are needed only for this comparative experiment. Native tests
and the Rust application do not require them. The frozen JavaScript reference
lives under `benchmarks/reference-js`.

These results apply to this machine and fixture. They do not predict live model
latency, quota availability, long-session memory, or exactly-once side effects.
Real provider startup and network generation remain outside harness control.

[Raw results](results/2026-11-14-rust-port.json). The earlier
[Node/Bun/Rust-probe study](RESULTS.md) is historical and predates the full port.
