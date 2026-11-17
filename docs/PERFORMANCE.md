# Performance measurement (task [071][M5], extended by issue #175)

This page records the measured **startup time**, **per-tool retrieval p95
latency**, the **worst-command guidance latency and its continuation**, and
**memory** of the `rust-docs-mcp` server, and compares them against the plan's
§11 provisional performance targets:

- **Startup** completes within **two seconds** for the full corpus.
- **Search and section reads** have **p95 latency below 100 ms** on a documented
  development machine.

Issue #175 extended the harness and this budget to the **guidance path** — the
multi-section `get_command_guidance` calls that the regression made take several
seconds — so the numbers now cover **all five tools**, the **worst supported
command** and its **continuation page**, over the shipped corpus and a documented
larger corpus. The §11 "below 100 ms" target is applied here as a **guidance
latency budget** (see [Guidance latency budget](#guidance-latency-budget-issue-175)).

> Status: the measurement harness ([`tests/perf.rs`](../tests/perf.rs)) is in
> place, gated, and produces the numbers below. The numbers were captured with
> the run command in [How to run the harness](#how-to-run-the-harness); a JSON
> copy is written when `RUST_DOCS_MCP_PERF_REPORT` names a path.

## Corpus measured

The harness drives the **real** `rust-docs-mcp` `serve` binary over spawned OS
stdio, over the **bundled seed** corpus — the `rust-docs-bundled-seed` snapshot
the binary always loads at compile time (`crate::bundled`) — by default, and,
when the operator pins `RUST_DOCS_MCP_PERF_DATA_DIR`, a **published snapshot in
that data dir** (a "larger" corpus). Each corpus is measured the **same way** and
reported **with its own identity** (snapshot id, parser version, section count,
content fingerprint, and per-source section counts), so a number can never be
attributed to the wrong corpus. It is fixed, offline, and hermetic: no network, a
per-run empty `--data-dir` (or the operator-pinned one), a fixed protocol
version, and fixed query/section sets and iteration counts.

The run recorded below measured the **shipped** (bundled) corpus. Its identity
changed since the original [071] numbers — the seed grew from **545** to
**877** sections — so those earlier figures (startup ≈ 10 ms, RSS ≈ 17 MiB)
describe a *smaller* corpus; the current figures below describe the one the server
ships today.

**Full-corpus / larger-corpus caveat.** The seed is a representative *offline
subset* of the upstream corpus, not the full live-synced corpus the product
serves in production. The numbers therefore describe the **shipped** corpus and
scale with it: a larger (live-synced) corpus increases both startup (more content
to index at boot) and peak RSS. The harness is **corpus-agnostic** — point it at
a live-synced corpus via `RUST_DOCS_MCP_PERF_DATA_DIR` and it measures that
corpus the same way, reporting that corpus's own identity — so the shipped
numbers are a **lower bound**, not a full-corpus guarantee. Producing that
live-synced larger corpus is an explicit, networked `sync` step (see
[Larger corpus](#larger-corpus-issue-175-ac2)). Correction round 2/2 ran that
leg: the live `sync` reached the network and fetched all eight sources' content,
but it **failed the production completeness gate** — the live ingestion pipeline
does not yet populate per-source attribution or a command catalog (both fields
the offline `bundled` seed builder does) — so it never published a larger snapshot
and `serve` fell back to the embedded seed. The larger-corpus *measurement* is
therefore a documented, ready-to-run step currently **blocked by that upstream
ingest gap**, not by this harness, and is not a number recorded here.

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

To measure the **larger** corpus, point the harness at a data dir holding a
live-synced published snapshot:

```sh
cargo build --release --locked
RUST_DOCS_MCP_PERF=1 RUST_DOCS_MCP_PERF_DATA_DIR=/path/to/live-synced-data-dir \
  RUST_DOCS_MCP_PERF_REPORT=/path/to/report.json \
  cargo test --release --test perf -- --ignored --nocapture
```

The report then carries **two** corpora — the shipped one and the pinned one —
each with its own identity, side by side.

## Methodology

- **Startup.** `STARTUP_RUNS` (10) fresh `serve` processes are spawned (a small
  warmup is discarded first so the OS loader cache does not skew the first
  sample). For each, the window is *process spawn → the `initialize` response is
  read* (the `Client::new` handshake, including `notifications/initialized`). The
  report records min / median / **p95** / max in milliseconds.
- **Retrieval p95 — all five tools.** Over a **fixed** set, each tool is issued
  `RETRIEVAL_ITERS` (30) times against one warm client:
  - `list_sources` — the catalog read;
  - `search_docs` — the fixed 8-query set (`limit: 5`);
  - `get_section` — the fixed 8-section set (one real section per source, by the
    id the *shipped* corpus actually stores — a fast guard,
    `perf_corpus_section_ids_are_real`, keeps these ids from drifting to stale
    placeholders, the bug issue #156 fixed);
  - `get_api_checklist` — the unfiltered "all" read plus each named category;
  - `get_command_guidance` — a **worst-command sweep** (see below).
  Before timing anything, the harness **asserts non-empty hits** for every query,
  a resolved section for every id, a non-empty checklist, and a resolved entry for
  every command, so a zero-hit or errored "fast" result can never masquerade as a
  retrieval. The report records the **p95** round-trip latency of each tool
  separately.
- **Worst-command guidance + continuation (the #175 regression path).** The
  command catalog is **enumerated from the served `list_sources` response** (not a
  hard-coded list that can drift). A first pass sweeps **every** command at the
  largest valid guidance page (`LARGE_GUIDANCE_LIMIT = 15` sections, ~30 KiB, just
  under the 32 KiB response cap — the largest page that does not overflow the
  cap) over a modest `GUIDANCE_COMMAND_ITERS` (10) to find the **slowest**
  command. That worst command is then re-measured at the full `RETRIEVAL_ITERS`
  (30) as the headline worst-case number, and its **continuation page** is
  measured the same way, resuming a real `next_cursor` captured from a truncated
  first page. The worst command in the shipped corpus is `rustfmt` (it cites ~50
  sections) — the exact call the issue #175 regression is about.
- **Memory.** The serving process's resident set size (RSS) is sampled via the
  platform `ps` tool once the corpus is loaded (the index is built at startup, so
  this is a representative peak). It is reported as an **approximation of peak
  RSS**, in KiB.
- **Percentiles** use a nearest-rank rule over the sorted samples, keeping the
  harness dependency-free and deterministic for a fixed iteration count.

## Measured numbers

Captured on **macOS** (arm64), **release** profile, `rustc 1.100.0
(0ef1bc386 2026-11-09)`, over the **877-section** shipped corpus
(`content_fingerprint` `6c4b9cd87aa68f82`), on the current development machine,
with the command in [How to run the harness](#how-to-run-the-harness). These are
real measurements from that run; they are machine- and corpus-dependent and will
differ on other hardware or a larger corpus. The corpus identity and environment
(below) pin which run a number came from.

| Metric | Measured (shipped corpus, 877 sections) | §11 target | Meets target? |
| --- | --- | --- | --- |
| Startup p95 (process spawn → `initialize`) | **31.46 ms** (min 30.81, median 31.15, max 31.46 ms over 10 runs) | ≤ 2000 ms | **Yes** (≈ 60× margin) |
| `list_sources` p95 round-trip | **2.36 ms** (30 iterations) | < 100 ms | **Yes** |
| `search_docs` p95 round-trip | **0.13 ms** (30 iterations × 8 queries) | < 100 ms | **Yes** |
| `get_section` p95 round-trip | **0.06 ms** (30 iterations × 8 sections) | < 100 ms | **Yes** |
| `get_api_checklist` p95 round-trip | **0.10 ms** (30 iterations × "all" + 1 category) | < 100 ms | **Yes** |
| `get_command_guidance` **worst command** (`rustfmt`) p95 | **0.96 ms** (30 iterations at the largest valid page) | < 100 ms | **Yes** (≈ 100× margin) |
| `get_command_guidance` **continuation** page p95 | **0.97 ms** (30 iterations, resuming a real cursor) | < 100 ms | **Yes** |
| Peak RSS (approximation) | **≈ 22.6 MiB** (23120 KiB) | not fixed by §11 | — |

All **29** commands in the served catalog were swept; every one is sub-100 ms
(`rustfmt` is the worst at ~1 ms, `cargo test`/`cargo add`/`cargo build` cluster
around ~0.4 ms, and the lightest commands are well under 0.1 ms). The full
per-command p95 table is in the JSON report below.

The JSON report for this run carries the new `corpora` array (so a number is
always attributed to its corpus), the per-tool `retrieval` block, the new
`guidance` block, and the `toolchain` / `host` fields:

```json
{
  "ok": true,
  "host": "macos aarch64",
  "profile": "release",
  "toolchain": "rustc 1.100.0 (0ef1bc386 2026-11-09)",
  "corpora": [
    {
      "label": "shipped",
      "corpus": {
        "label": "shipped-bundled-seed",
        "snapshot_id": "rust-docs-bundled-seed",
        "parser_version": "rust-docs-mcp/seed-2",
        "section_count": 877,
        "content_fingerprint": "6c4b9cd87aa68f82",
        "corpus_hash": "c128-ffaf654d6d447eb40b9e9629be845a92",
        "command_count": 29,
        "sources": [
          "api-guidelines", "style-guide", "cargo", "rustc",
          "rustup", "rustfmt", "clippy", "rustdoc"
        ]
      },
      "startup": {
        "runs": 10,
        "min_ms": 30.81,
        "median_ms": 31.15,
        "p95_ms": 31.46,
        "max_ms": 31.46,
        "target_ms": 2000.0,
        "meets_target": true
      },
      "retrieval": {
        "iters_per_item": 30,
        "queries": 8,
        "sections": 8,
        "list_sources_p95_ms": 2.36,
        "search_p95_ms": 0.13,
        "section_p95_ms": 0.06,
        "checklist_p95_ms": 0.10,
        "target_ms": 100.0,
        "meets_target": true
      },
      "guidance": {
        "commands_measured": 29,
        "worst_command": "rustfmt",
        "worst_p95_ms": 0.96,
        "continuation_p95_ms": 0.97,
        "target_ms": 100.0,
        "meets_target": true,
        "per_command_p95_ms": [
          { "command": "cargo test", "p95_ms": 0.45 },
          { "command": "cargo add", "p95_ms": 0.4 },
          { "command": "rustfmt", "p95_ms": 1.04 },
          { "command": "rustc", "p95_ms": 0.54 }
        ],
        "note": "issue #175: the worst command's guidance is the multi-section regression; \
 it is now sub-100 ms (was ~3.4 s before the index reuse fix)"
      },
      "memory": {
        "peak_rss_kib": 23120,
        "note": "approximation: sampled once after the corpus is loaded"
      }
    }
  ]
}
```

> The `per_command_p95_ms` block above is abbreviated to a few representative
> commands (the worst, `rustfmt`, and a few `cargo` subcommands); the JSON report
> written by the harness contains all **29** entries.

## Guidance latency budget (issue #175)

Issue #175 reported that `get_command_guidance({"command":"rustfmt"})` took
**several seconds** to return its first five sections — the regression the page
now budgets. This section records the **before/after** and states the budget the
harness enforces.

### The regression (measured *before* the fix)

From issue #175, measured on the README-installed **release** binary, same warm
stdio session, over the then-shipped **545-section** corpus:

- Three repeated `rustfmt` guidance calls: **3418 ms, 3513 ms, 3393 ms** — each
  returned only five sections plus a continuation. (≈ **3.4 s** per call.)
- `cargo test` guidance: **262 ms**.
- A separate release harness measured startup p95 **8.97 ms**, search p95
  **13.55 ms**, section p95 **28.27 ms** — isolating the cost to the guidance
  path, not process start or a network dependency.

The cause was that each cited section's read independently rebuilt the entire
lexical section index (`SectionIndex::build`), so a multi-section guidance call
redid `O(corpus)` work **once per cited section** — for `rustfmt`, ~50 times per
call.

### The budget

Every guidance call — **all five tools**, the **worst supported command**, and
its **continuation page** — is held to the plan's §11 retrieval target:

> **Guidance latency budget:** `get_command_guidance` p95 round-trip is
> **< 100 ms** for the worst command *and* its continuation, on the recorded
> machine, with no whole-corpus index rebuild per cited section.

This is the §11 "p95 below 100 ms" target extended to the guidance path the
regression affected. The harness asserts it directly: the `guidance.meets_target`
field is `true` only when the worst command's p95 **and** its continuation p95 are
both ≤ 100 ms.

### The result (measured *after* the fix)

Measured by the current harness on the **877-section** shipped corpus (a corpus
**larger** than the one the regression was recorded against):

| | Before the fix (545 sections) | After the fix (877 sections) |
| --- | --- | --- |
| `rustfmt` guidance, first page | **≈ 3.4 s** (3393–3513 ms) | **0.96 ms** (worst command) |
| `rustfmt` guidance, continuation | not separately measured | **0.97 ms** |
| `cargo test` guidance | **262 ms** | **0.45 ms** |
| Worst of all 29 commands | — | **0.96 ms** (`rustfmt`) |

**The multi-second `rustfmt` regression is eliminated.** The worst command's
guidance is now **~0.96 ms** — roughly **3400×** faster than the pre-fix ~3.4 s —
and it holds on a corpus that is **60% larger** than the one the regression was
recorded on, because the section index is now built **once per snapshot** and
shared across handlers instead of rebuilt per cited section. The continuation
page resolves through the **same** shared index at the same sub-millisecond p95.
Every command in the served catalog is sub-100 ms, with large margin.

## Larger corpus (issue #175 AC2)

The harness measures a second, **larger** (live-synced) corpus the same way when
the operator pins `RUST_DOCS_MCP_PERF_DATA_DIR` to a data dir holding a published
snapshot — see the second run command in [How to run the harness](#how-to-run-the-harness).
The report then carries **two** corpora side by side, each with its own identity,
so the larger-corpus numbers are never conflated with the shipped ones.

**What was measured, and why the larger-corpus measurement is blocked.** The
shipped-corpus numbers above are **measured** (all five tools, the worst command,
its continuation, and peak RSS). The larger-corpus path is **implemented and
corpus-agnostic**, and correction round 2/2 **ran the networked `sync` leg** to
produce one. The attempt:

- **Network reached.** `rust-docs-mcp sync --data-dir <temp>` connected to the
  four upstream hosts the registry vouches for (`doc.rust-lang.org`,
  `rust-lang.github.io`, `raw.githubusercontent.com`, `clippy.rust-lang.org` —
  all reachable) and fetched real content for **all eight** sources (the gate
  reported *content* for each one).
- **The completeness gate aborted publication.** The run failed the production
  completeness gate with nine gaps: *the command catalog is empty*, and *every
  source “has content but no attribution record.”* Both are fields the live
  pipeline (`src/ingest/pipeline.rs`) does **not** populate — it hardcodes an
  empty attribution list (`attribution: Vec::new()`) and derives its command
  catalog from `from_sections` over the live sections, which came back empty —
  whereas the offline `bundled` seed builder (`src/bundled.rs`) populates both.
  A production `sync` (`require_complete = true`) therefore publishes **nothing**:
  the data dir was left empty (no `corpus/` directory), and `serve` pointed at it
  fell back to the embedded **877-section seed** (verified: the reported per-source
  content hashes and `2026-10-30` fetch dates are the seed's, not a live corpus's).
- **No larger snapshot, so no larger measurement.** Because `serve`'s runtime
  selection runs the *same* completeness gate (`runtime::gate_completeness`), it
  would reject any larger-but-incomplete snapshot in favor of the seed, so a
  larger corpus the harness could point at is **not obtainable through the shipped
  `sync`** at this point.

**This is a separate ingest-pipeline gap, not a defect in the #175 index-reuse
fix.** That fix is correct and regression-free (verified independently on the
shipped corpus); the blocker is that the live `sync` pipeline does not yet
populate per-source attribution or a command catalog the way the offline seed
builder does. The full-corpus figures are expected to be higher than the shipped
ones (more content indexed at boot, more held in memory); once the live `sync`
populates those fields, the **same** corpus-agnostic harness (the second run
command in [How to run the harness](#how-to-run-the-harness)) measures the larger
corpus with no change, and the full-corpus numbers can be recorded here alongside
the shipped figures above.

## Comparison against the §11 targets

All five tools and the guidance path meet the §11 targets **with large margin**
on the shipped corpus:

- **Startup (≤ 2000 ms).** Measured p95 is **31.46 ms** — roughly two orders of
  magnitude under the two-second target. The startup window includes process
  spawn, the `initialize` handshake, and loading/indexing the shipped corpus.
  (Startup rose from the original ≈ 10 ms as the corpus grew from 545 to 877
  sections — the expected scaling — yet remains ~60× under target.)
- **Retrieval p95 (< 100 ms).** Measured p95 is **0.06–2.36 ms** across
  `search_docs`, `get_section`, `get_api_checklist`, and `list_sources` — well
  under the 100 ms target. These are sub-millisecond because the retrieval is a
  localhost stdio pipe round-trip over an in-memory index of the shipped corpus,
  and the harness drains the child's stdout and asserts a graceful self-exit per
  measurement.
- **Guidance p95 (< 100 ms) — the #175 regression.** Measured worst-command
  (`rustfmt`) p95 is **0.96 ms** and its continuation **0.97 ms**; the pre-fix
  ~3.4 s regression is eliminated (see
  [Guidance latency budget](#guidance-latency-budget-issue-175)).
- **Memory.** Peak RSS is **≈ 22.6 MiB**. §11 does not fix a memory target, so
  this is reported for the record, not compared.

Because the shipped corpus is small, the margins are large. The meaningful test
is the **full live-synced corpus**: the same harness, pointed at a live-synced
corpus via `RUST_DOCS_MCP_PERF_DATA_DIR`, produces the full-corpus figures
(expected to be higher, since more content is indexed at boot and held in
memory). Until such a corpus is available, the shipped-corpus figures
demonstrate that the server's startup, retrieval, and guidance paths meet the §11
targets by a wide margin, and the harness is ready to measure the full corpus the
same way.
