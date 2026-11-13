# Performance measurement (task [071][M5])

This page records the measured **startup time**, **p95 retrieval latency**, and
**memory** of the `rust-docs-mcp` server, and compares them against the plan's
§11 provisional performance targets:

- **Startup** completes within **two seconds** for the full corpus.
- **Search and section reads** have **p95 latency below 100 ms** on a documented
  development machine.

> Status: the measurement harness ([`tests/perf.rs`](../tests/perf.rs)) is in
> place and gated. The measured numbers are being captured and will be recorded
> below. The corpus measured, the methodology, and any target revision are
> documented here so the numbers are reproducible.

## Corpus measured

The harness drives the real `rust-docs-mcp` `serve` binary over spawned OS stdio,
over the **bundled seed** corpus — the `rust-docs-bundled-seed` snapshot the
binary always loads at compile time (`crate::bundled`). It is fixed, offline, and
hermetic: no network, a per-run empty `--data-dir`, a fixed protocol version, and
fixed query/section sets and iteration counts.

_Caveat and full-corpus note are recorded in the final report below._

## How to run the harness

The harness is gated so it never slows the normal `cargo test` suite:

```sh
cargo build --release --locked
RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored -- --nocapture
```

The `--release` flag matters: the harness measures the binary the current `cargo`
invocation built, so a release run measures the release binary. Set
`RUST_DOCS_MCP_PERF_REPORT=/path/to/report.json` to also write a JSON copy.

## Measured numbers

_Recorded after the harness is run. See the methodology and comparison below._
