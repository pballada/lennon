# Lennon runtime benchmark

This experiment compares Node.js, optional Bun, and a release-mode Rust probe.
It measures the code rather than assuming one language wins every workload.
The Rust executable is a **benchmark probe, not a complete Lennon port**.

## Reproduce

Prerequisites: Node 22+, a Rust toolchain, and optionally Bun. Rust's only
benchmark dependency is locked in `rust/Cargo.lock`; Lennon itself keeps zero
runtime dependencies. Nothing invokes a live model or reads login credentials.

```sh
cargo build --release --locked --manifest-path benchmarks/rust/Cargo.toml
node benchmarks/run.mjs benchmarks/results/local.json

# Optional: measure exactly the same JS under Bun too.
LENNON_BENCH_BUN=/absolute/path/to/bun \
  node benchmarks/run.mjs benchmarks/results/local-with-bun.json

# macOS: correct the runtime disk-flush mismatch found in the first experiment.
LENNON_BENCH_BUN=/absolute/path/to/bun \
  node benchmarks/equal-durability.mjs benchmarks/results/equal-durability.json

node benchmarks/report.mjs benchmarks/results/local-with-bun.json
```

`LENNON_BENCH_RUST` can override the release binary location.
`LENNON_BENCH_RUST_VERSION` records the compiler version in the result metadata.
The script does not install or switch the system's runtimes.

## Workloads

| Workload | What it measures | Samples per variant |
| --- | --- | ---: |
| Startup | Fresh process, tiny `--help` probe; shipping Lennon measured separately | 100 |
| JSON | Read, parse, inspect, and serialize 100,000 mixed JSONL events | 25 |
| Durable journal | Same work for 250 events, with `fsync` after **every** event | 20 |
| Descriptor reuse | Same durable journal, keeping the descriptor open between events | 20 |
| Shipping failover | Full Lennon: native mock Claude streams 100 events, hits quota, native mock Codex receives the transcript and completes | 30 |
| Memory | Peak RSS from macOS `/usr/bin/time -l`; measured separately | 5 |

Five warmups precede each variant's measurements. Run order is randomized within
balanced blocks using a fixed seed. Samples, medians, p95, extrema, input sizes,
and output validations are stored in the JSON artifact. No outliers are removed.
Filesystem caches are warm; these are **fresh-process**, not cold-disk measurements.

The JS probe uses V8/JavaScriptCore's native JSON parser. Rust uses `serde_json`
with insertion-order preservation and serializes the parsed event by reference.
Both variants inspect the same fields and write byte-identical journal records;
SHA-256 equality is checked outside the timed region. Calling the same API did
**not** guarantee the same durability: Node's libuv and Rust's `sync_all` use
`F_FULLFSYNC` on macOS, whereas Bun 1.4.4's `fsyncSync` calls plain `fsync`.
The initial Bun journal and failover numbers are retained but must not be treated
as equal-durability wins.

`equal-durability.mjs` corrects this with a benchmark-only Bun FFI call to
`fcntl(fd, F_FULLFSYNC)`. It tests 100 journal events (15 samples) and complete
100-event failovers (20 samples), with five warmups and randomized blocks. A
temporary copy of Lennon changes only the imported disk-sync binding; production
source is untouched. The shim refuses failure instead of silently using a weaker
flush. Neither implementation batches event syncs. The experimental FFI shim
does not ship in Lennon and is not a production runtime recommendation.

Versioned source evidence:

- [Node's libuv 1.53.0](https://github.com/libuv/libuv/blob/v1.53.0/src/unix/fs.c#L158)
- [Rust 1.100.0 sync implementation](https://github.com/rust-lang/rust/blob/1.100.0/library/std/src/sys/fs/unix.rs#L1185)
- [Bun 1.4.4 fsync implementation](https://github.com/oven-sh/bun/blob/bun-v1.4.4/src/runtime/node/node_fs.rs#L5283)

The actual failover test is **shipping Lennon's JavaScript**, under Node and Bun
when selected. It validates that the fallback receives the task and streamed
history and that Lennon emits a successful Codex completion. The handoff interval
starts when the mock primary emits quota and ends when the fallback has read
the complete handoff prompt. Total time includes launch, journaling, metadata,
native process startup, and completion. The mocks use the same Rust binary for
both JS runtimes so an extra Node mock process cannot bias the comparison.

## Limits

- A help-only Rust probe has fewer features than full Lennon. Compare it with
  the JS help-only probe to isolate runtime startup, not to claim a production
  Rust implementation is 100% feature-complete.
- JSON and file I/O are isolated hot-path probes. They do not prove identical
  full-application performance or optimal implementations in every language.
- Rust failover is not measured: implementing a fully equivalent native harness
  would require a separate port with session, permission, and recovery validation.
- Live provider startup, network/model latency, throttling, production session
  growth, and terminal rendering are not part of these controlled results.
- Background OS work and storage variability still exist despite randomization.
- Bun passing mock tests does not establish live provider or cross-platform parity.

See `RESULTS.md` for the measured decision and `results/` for the raw data.
