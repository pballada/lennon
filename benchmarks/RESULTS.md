# Runtime measurements · 2026-11-14

Measured on Apple M4, darwin/arm64,
OS kernel 25.6.0. Node v26.10.0; Bun 1.4.4;
rustc 1.100.0 (0ef1bc386 2026-11-09). Release Rust build with LTO.

## Controlled results

Times are milliseconds; smaller is faster. Wall time includes a fresh process.
Work time starts inside the process and excludes runtime startup. Each row uses
five warmups and balanced randomized samples, with no outlier removal.

| Workload | Implementation | Wall median | Wall p95 | Work median |
| --- | --- | ---: | ---: | ---: |
| Fresh-process startup | node_probe | 42.14 | 44.65 | — |
| Fresh-process startup | rust_probe | 1.70 | 2.05 | — |
| Fresh-process startup | lennon_full | 47.08 | 52.30 | — |
| Fresh-process startup | bun_probe | 6.19 | 6.91 | — |
| Fresh-process startup | lennon_bun | 11.89 | 13.19 | — |
| 100k JSON events | node_parse | 152.32 | 154.91 | 107.21 |
| 100k JSON events | bun_parse | 73.55 | 75.17 | 64.58 |
| 100k JSON events | rust_parse | 96.52 | 106.44 | 94.40 |

"probe" variants implement the same tiny help task. Full Lennon help loads the
actual production modules. Rust's JSON/journal probe implements the measured
hot path, not the rest of Lennon. It is not a feature-equivalent Rust product.

Every journal candidate requested fsync after every event, and all journal
SHA-256 digests and event counters matched. However, the underlying operations
were different: macOS Node and Rust request F_FULLFSYNC, while Bun 1.4.4 calls
plain fsync. Initial Bun journaling results are **not equal-durability wins**.
The journal-open variant retains the descriptor and still syncs every event.

## Corrected comparison: identical macOS full-sync policy

The follow-up explicitly uses F_FULLFSYNC in each runtime after every event.
Its Bun shim is benchmark-only; no production source or default runtime changed.
Journal output hashes match. Fifteen samples per journal variant, five warmups.

| 100-event journal | Wall median | Wall p95 | Work median |
| --- | ---: | ---: | ---: |
| node_journal | 471.44 | 477.11 | 380.43 |
| rust_journal | 385.56 | 392.16 | 379.02 |
| bun_fullsync_journal | 401.58 | 417.58 | 378.76 |
| node_journal-open | 467.33 | 474.88 | 375.92 |
| rust_journal-open | 381.63 | 387.48 | 375.21 |
| bun_fullsync_journal-open | 401.09 | 411.06 | 377.37 |

For the complete pipeline, the Bun overlay changes only the imported sync
binding in a temporary copy of Lennon. Twenty samples per runtime, five warmups.

| Equal-durability runtime | Total median | Total p95 | Handoff median | Handoff p95 |
| --- | ---: | ---: | ---: | ---: |
| node | 577.89 | 592.02 | 422.31 | 437.05 |
| bun_fullsync | 527.55 | 538.33 | 418.31 | 427.14 |

## Actual Lennon automatic failover

The initial, uncorrected runs use the shipping harness, native mock providers, 100 streamed
primary events, an explicit quota failure, and successful Codex completion.
The fallback checks that the user request and streamed context reached it.
Handoff measures quota emission until the next provider has read the full prompt.

| Runtime | Total median | Total p95 | Handoff median | Handoff p95 |
| --- | ---: | ---: | ---: | ---: |
| node | 549.27 | 586.71 | 421.30 | 441.59 |
| bun | 55.44 | 57.61 | 9.46 | 9.96 |

These initial Bun timings include weaker disk flushes and are not comparable
at the same durability. Use the corrected results above for that comparison.
No equivalent full Rust harness has been implemented or measured.

## Memory

Peak RSS in MiB, median of five separate macOS time(1) samples. Parse probes
process the same 100,000-event file. This is not a long-running session memory test.

| Variant | Peak RSS median |
| --- | ---: |
| node_probe | 118.48 |
| rust_probe | 20.83 |
| lennon_help | 48.13 |
| bun_probe | 189.94 |

## Uncertainty

Paired median differences with a 95% percentile bootstrap interval (10,000
resamples). A positive value means the second candidate is faster in this
experiment. This estimates sampling variation, not performance on other machines.

| Comparison | Median difference [95% bootstrap interval] |
| --- | ---: |
| Probe startup: Node minus rust | 40.45 ms [40.15, 40.75] |
| Probe startup: Node minus bun | 36.04 ms [35.78, 36.24] |
| 100k JSON: Node minus rust | 55.52 ms [51.91, 56.87] |
| 100k JSON: Node minus bun | 78.80 ms [77.46, 80.33] |
| Full Lennon startup: Node minus Bun | 35.39 ms [35.15, 35.61] |
| Equal full-sync journal: Node minus Rust | 85.80 ms [79.82, 89.38] |
| Equal full-sync journal: Node minus Bun | 67.51 ms [57.04, 71.82] |
| Equal full-sync handoff: Node minus Bun | 5.30 ms [1.19, 10.43] |

## Decision

Rust wins the minimal startup test. That alone does not prove a Rust rewrite
would deliver the fastest complete coding harness. JSON processing, journaling,
and provider lifecycle behavior need separate evidence, as shown above.

The primary measured streaming bottleneck is synchronous full-disk flushing
per event. Changing the language does not remove that cost. Descriptor reuse
alone does not remove it either. Investigate journal flush scheduling before
committing to a full rewrite; any batching needs an explicit durability contract.

Bun wins this JSON workload and speeds up full CLI startup without rewriting
the application. It passed the 18 mock/subprocess tests on this machine, but
its default macOS flush behavior differs. Keep Node as the supported default
until durability, live providers, and cross-platform behavior are validated.
Rust is the strongest measured candidate for minimal startup and low memory,
not a demonstrated full-harness winner. A full Rust port still needs session,
permissions, streaming, and recovery parity validation.

Do not extrapolate mock model timing to live provider/model latency. No real
quota exhaustion or live network generation was used in this experiment.
The findings apply to these candidates and this machine; they are not proof
of a universally fastest implementation.

Reproduction and limitations: [README.md](README.md). Raw samples, versions,
digests, counters, and distributions: [initial run](results/2026-11-14-macos-arm64.json)
and [equal-durability follow-up](results/2026-11-14-macos-arm64-fullsync.json).

The runtime source evidence explaining the sync mismatch is linked in
[the methodology](README.md#workloads).
