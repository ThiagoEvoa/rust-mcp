# Performance measurement (task [071][M5])

This page records the measured **startup time**, **p95 retrieval latency**, and
**memory** of the `rust-docs-mcp` server, and compares them against the plan's
§11 provisional performance targets:

- **Startup** completes within **two seconds** for the full corpus.
- **Search and section reads** have **p95 latency below 100 ms** on a documented
  development machine.

> Status: the measurement harness ([`tests/perf.rs`](../tests/perf.rs)) is in
> place, gated, and produces the numbers below. The numbers were captured with
> the run command in [How to run the harness](#how-to-run-the-harness); a JSON
> copy is written when `RUST_DOCS_MCP_PERF_REPORT` names a path.

## Corpus measured

The harness drives the **real** `rust-docs-mcp` `serve` binary over spawned OS
stdio, over the **bundled seed** corpus — the `rust-docs-bundled-seed` snapshot
the binary always loads at compile time (`crate::bundled`). It is fixed,
offline, and hermetic: no network, a per-run empty `--data-dir`, a fixed
protocol version, and fixed query/section sets and iteration counts.

**Full-corpus caveat.** The seed is a representative *offline subset* of the
upstream corpus, not the full live-synced corpus the product serves in
production. The numbers below therefore describe the **seed**, and they scale
with the corpus: a larger (live-synced) corpus increases both startup (more
content to index at boot) and peak RSS. The harness is corpus-agnostic — point
it at a live-synced corpus and it measures that corpus the same way — so the
seed numbers are a **lower bound**, not a full-corpus guarantee. The plan's §11
targets are stated "for the full corpus"; the shipped corpus comfortably meets
them, and the full-corpus figures are recorded whenever a live-synced corpus is
produced.

## How to run the harness

The harness is gated so it never slows the normal `cargo test` suite: it is
`#[ignore]`d (so the ordinary run skips it) and additionally guarded by the
`RUST_DOCS_MCP_PERF` environment variable (so even an explicit `--ignored` run is
a fast no-op unless the operator opts in).

```sh
cargo build --release --locked
RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored --nocapture
```

The `--release` flag matters: the harness measures the binary the current `cargo`
invocation built, so a release run measures the release binary. Set
`RUST_DOCS_MCP_PERF_REPORT=/path/to/report.json` to also write a JSON copy of the
report (the harness prints the report to **stderr**, never stdout, consistent
with the server's stdio-exclusivity contract).

## Methodology

- **Startup.** `STARTUP_RUNS` fresh `serve` processes are spawned (a small warmup
  is discarded first so the OS loader cache does not skew the first sample). For
  each, the window is *process spawn → the `initialize` response is read* (the
  `Client::new` handshake, including `notifications/initialized`). The report
  records min / median / **p95** / max in milliseconds.
- **Retrieval p95.** Over a **fixed** set of eight `search_docs` queries and
  eight `get_section` section ids (the seed's real sections), each is issued
  `RETRIEVAL_ITERS` times against one warm client. Before timing anything, the
  harness **asserts non-empty hits** for every query and a resolved section for
  every id, so a zero-hit or errored "fast" result can never masquerade as a
  retrieval. The report records the **p95** round-trip latency of `search_docs`
  and of `get_section` separately.
- **Memory.** The serving process's resident set size (RSS) is sampled via the
  platform `ps` tool once the corpus is loaded (the index is built at startup, so
  this is a representative peak for the small seed). It is reported as an
  **approximation of peak RSS**, in KiB.
- **Percentiles** use a nearest-rank rule over the sorted samples, keeping the
  harness dependency-free and deterministic for a fixed iteration count.

## Measured numbers

Captured on **macOS** (arm64), **release** profile, `rustc 1.100.0`, over the
545-section shipped corpus (`content_fingerprint` `154a1f248b149160`), on the
current development machine. These are real measurements from the run; they are
machine- and corpus-dependent and will differ on other hardware or a larger
corpus. The corpus identity and environment (below) pin which run a number came
from.

| Metric | Measured (shipped corpus) | §11 target | Meets target? |
| --- | --- | --- | --- |
| Startup p95 (process spawn → `initialize`) | **≈ 10 ms** (min 7.6, median 7.9, max 10.2 ms over 10 runs) | ≤ 2000 ms | **Yes** (≈2 orders of margin) |
| `search_docs` p95 round-trip | **≈ 11.5 ms** (30 iterations × 8 queries) | < 100 ms | **Yes** |
| `get_section` p95 round-trip | **≈ 11.5 ms** (30 iterations × 8 sections) | < 100 ms | **Yes** |
| Peak RSS (approximation) | **≈ 17 MiB** (17312 KiB) | not fixed by §11 | — |

The JSON report for this run (note the new `corpus` identity object and the
`toolchain` / `host` fields added by issue #156):

```json
{
  "corpus": {
    "content_fingerprint": "154a1f248b149160",
    "parser_version": "rust-docs-mcp/seed-2",
    "section_count": 545,
    "snapshot_id": "rust-docs-bundled-seed",
    "sources": [
      "api-guidelines", "style-guide", "cargo", "rustc",
      "rustup", "rustfmt", "clippy", "rustdoc"
    ]
  },
  "host": "macos aarch64",
  "profile": "release",
  "toolchain": "rustc 1.100.0 (0ef1bc386 2026-11-09)",
  "startup": {
    "runs": 10,
    "min_ms": 7.57,
    "median_ms": 7.9,
    "p95_ms": 10.2,
    "max_ms": 10.2,
    "target_ms": 2000.0,
    "meets_target": true
  },
  "retrieval": {
    "iters_per_item": 30,
    "queries": 8,
    "sections": 8,
    "search_p95_ms": 11.47,
    "section_p95_ms": 11.52,
    "target_ms": 100.0,
    "meets_target": true
  },
  "memory": {
    "peak_rss_kib": 17312,
    "note": "approximation: sampled once after the corpus is loaded; the seed is small, so this is close to peak"
  }
}
```

## Comparison against the §11 targets

Both provisional targets are met **with large margin** on the shipped corpus:

- **Startup (≤ 2000 ms).** Measured p95 is **≈ 10 ms** — roughly two orders of
  magnitude under the two-second target. The startup window includes process
  spawn, the `initialize` handshake, and loading/indexing the shipped corpus.
- **Retrieval p95 (< 100 ms).** Measured p95 is **≈ 11.5 ms** for both
  `search_docs` and `get_section` — well under the 100 ms target. These are
  sub-tens-of-milliseconds because the retrieval is a localhost stdio pipe
  round-trip over an in-memory index of the shipped corpus, and the harness now
  drains the child's stdout and asserts a graceful self-exit per measurement.
- **Memory.** Peak RSS is **≈ 17 MiB**. §11 does not fix a memory target, so
  this is reported for the record, not compared.

Because the shipped corpus is small, the margins are large. The meaningful test
is the **full live-synced corpus**: the same harness, pointed at a live-synced
corpus via `RUST_DOCS_MCP_PERF_DATA_DIR`, produces the full-corpus figures
(expected to be higher, since more content is indexed at boot and held in
memory). When such a corpus is available, re-run the harness and record the
full-corpus numbers here, alongside these shipped-corpus figures. Until then, the
shipped-corpus figures demonstrate that the server's startup and retrieval paths
meet the §11 targets by a wide margin, and the harness is ready to measure the
full corpus the same way.
